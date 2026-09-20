use crate::{
    database::{Database, RequestLogEntry},
    models::{ProxyRecord, ServerEvent},
    state::now_millis,
};
use anyhow::Result;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    thread,
    time::Duration,
};
use tokio::sync::broadcast;

const CAPACITY: usize = 2048;
const FAILURE_RESERVE: usize = 256;
const BATCH_SIZE: usize = 128;

pub struct OwnedLog {
    pub proxy_id: Option<i64>,
    pub host: String,
    pub port: i64,
    pub success: bool,
    pub time: Option<i64>,
    pub error: Option<String>,
    pub kind: String,
}
impl From<RequestLogEntry<'_>> for OwnedLog {
    fn from(log: RequestLogEntry<'_>) -> Self {
        Self {
            proxy_id: log.proxy_id,
            host: log.target_host.into(),
            port: log.target_port,
            success: log.success,
            time: log.response_time,
            error: log.error_message.map(str::to_string),
            kind: log.result_type.into(),
        }
    }
}
impl OwnedLog {
    pub fn entry(&self) -> RequestLogEntry<'_> {
        RequestLogEntry {
            proxy_id: self.proxy_id,
            target_host: &self.host,
            target_port: self.port,
            success: self.success,
            response_time: self.time,
            error_message: self.error.as_deref(),
            result_type: &self.kind,
        }
    }
}
enum Job {
    Log(OwnedLog),
    Status(Box<ProxyRecord>, u64, u64, String, Option<i64>),
}
impl Job {
    fn important(&self) -> bool {
        !matches!(self, Self::Log(log) if log.success)
    }
}
#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    enqueued: VecDeque<std::time::Instant>,
    stopped: bool,
    working: bool,
    dropped_logs: u64,
    errors: u64,
    written: u64,
    max_wait_ms: u64,
    max_queue_wait_ms: u64,
    peak_queue_length: usize,
}
struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    drained: Condvar,
}
struct Worker {
    db: Database,
    shared: Arc<Shared>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shared
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopped = true;
        self.shared.wake.notify_one();
        if let Some(join) = self
            .join
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            let _ = join.join();
        }
    }
}
#[derive(Clone)]
pub struct DatabaseWorker(Arc<Worker>);
impl DatabaseWorker {
    pub fn new(db: Database, events: broadcast::Sender<ServerEvent>) -> Result<Self> {
        let writer = db.background_connection()?;
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            drained: Condvar::new(),
        });
        let state = shared.clone();
        let status_db = db.clone();
        let join = thread::Builder::new()
            .name("proxy-db-writer".into())
            .spawn(move || {
                let mut last_event = std::time::Instant::now() - Duration::from_secs(1);
                loop {
                    let mut queue = state.queue.lock().unwrap_or_else(|e| e.into_inner());
                    while queue.jobs.is_empty() && !queue.stopped {
                        queue = state.wake.wait(queue).unwrap_or_else(|e| e.into_inner());
                    }
                    if queue.jobs.is_empty() && queue.stopped {
                        break;
                    }
                    // Coalesce bursts, bounded by 100 ms; exit flush skips the delay.
                    if queue.jobs.len() < BATCH_SIZE && !queue.stopped {
                        queue = state
                            .wake
                            .wait_timeout(queue, Duration::from_millis(100))
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                    }
                    let count = queue.jobs.len().min(BATCH_SIZE);
                    let batch = queue.jobs.drain(..count).collect::<Vec<_>>();
                    for enqueued in queue.enqueued.drain(..count).collect::<Vec<_>>() {
                        queue.max_queue_wait_ms = queue
                            .max_queue_wait_ms
                            .max(enqueued.elapsed().as_millis() as u64);
                    }
                    queue.working = true;
                    drop(queue);
                    let start = std::time::Instant::now();
                    let mut logs = Vec::new();
                    let mut errors = 0;
                    for job in batch {
                        match job {
                            Job::Log(log) => logs.push(log),
                            Job::Status(proxy, generation, revision, status, time) => {
                                if let Err(error) = db.update_passive_status(
                                    &proxy, generation, revision, &status, time,
                                ) {
                                    eprintln!("后台状态持久化失败: {error:#}");
                                    errors += 1;
                                }
                            }
                        }
                    }
                    let written = match writer.log_requests(&logs) {
                        Ok(count) => count as u64,
                        Err(error) => {
                            eprintln!("后台日志批写失败: {error:#}");
                            errors += 1;
                            0
                        }
                    };
                    let mut queue = state.queue.lock().unwrap_or_else(|e| e.into_inner());
                    queue.errors += errors;
                    queue.dropped_logs += logs.len() as u64 - written;
                    queue.written += written;
                    queue.max_wait_ms = queue.max_wait_ms.max(start.elapsed().as_millis() as u64);
                    queue.working = false;
                    state.drained.notify_all();
                    drop(queue);
                    if (written != 0 && last_event.elapsed() >= Duration::from_millis(250))
                        || errors != 0
                    {
                        last_event = std::time::Instant::now();
                        let _ = events.send(ServerEvent {
                            event_type: "request_logged".into(),
                            data: Value::Null,
                            timestamp: now_millis(),
                        });
                    }
                }
            })?;
        Ok(Self(Arc::new(Worker {
            db: status_db,
            shared,
            join: Mutex::new(Some(join)),
        })))
    }
    fn enqueue(&self, job: Job) -> bool {
        let mut queue = self
            .0
            .shared
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let limit = if job.important() {
            CAPACITY
        } else {
            CAPACITY - FAILURE_RESERVE
        };
        if queue.stopped || queue.jobs.len() >= limit {
            if job.important() && !queue.stopped {
                if let Some(index) = queue.jobs.iter().position(|queued| !queued.important()) {
                    queue.jobs.remove(index);
                    queue.enqueued.remove(index);
                    queue.dropped_logs += 1;
                } else {
                    queue.dropped_logs += 1;
                    return false;
                }
            } else {
                queue.dropped_logs += 1;
                return false;
            }
        }
        queue.jobs.push_back(job);
        queue.enqueued.push_back(std::time::Instant::now());
        queue.peak_queue_length = queue.peak_queue_length.max(queue.jobs.len());
        if queue.jobs.len() == 1 || queue.jobs.len() >= BATCH_SIZE {
            self.0.shared.wake.notify_one();
        }
        true
    }
    pub fn log(&self, entry: RequestLogEntry<'_>) {
        self.enqueue(Job::Log(entry.into()));
    }
    pub fn status(
        &self,
        proxy: ProxyRecord,
        generation: u64,
        status: &str,
        time: Option<i64>,
    ) -> bool {
        let revision = self.0.db.reserve_status_revision(proxy.id);
        self.enqueue(Job::Status(
            Box::new(proxy),
            generation,
            revision,
            status.into(),
            time,
        ))
    }
    pub fn flush(&self, budget: Duration) -> bool {
        self.0.shared.wake.notify_one();
        let queue = self
            .0
            .shared
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (queue, _) = self
            .0
            .shared
            .drained
            .wait_timeout_while(queue, budget, |q| !q.jobs.is_empty() || q.working)
            .unwrap_or_else(|e| e.into_inner());
        queue.jobs.is_empty() && !queue.working
    }
    pub fn stats(&self) -> Value {
        let queue = self
            .0
            .shared
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        json!({"queueLength": queue.jobs.len(), "peakQueueLength": queue.peak_queue_length, "maxQueueWaitMs": queue.max_queue_wait_ms, "capacity": CAPACITY, "droppedLogs": queue.dropped_logs, "databaseErrors": queue.errors, "writtenLogs": queue.written, "maxBatchDurationMs": queue.max_wait_ms})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_queue_reserves_space_and_evicts_regular_logs_for_failures() {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            drained: Condvar::new(),
        });
        let worker = DatabaseWorker(Arc::new(Worker {
            db: Database::open_in_memory().unwrap(),
            shared,
            join: Mutex::new(None),
        }));
        let entry = |success| RequestLogEntry {
            proxy_id: None,
            target_host: "test",
            target_port: 80,
            success,
            response_time: None,
            error_message: None,
            result_type: "test",
        };
        for _ in 0..CAPACITY * 2 {
            worker.log(entry(true));
        }
        assert_eq!(
            worker.stats()["queueLength"],
            json!(CAPACITY - FAILURE_RESERVE)
        );
        for _ in 0..FAILURE_RESERVE + 1 {
            worker.log(entry(false));
        }
        let queue = worker.0.shared.queue.lock().unwrap();
        assert_eq!(queue.jobs.len(), CAPACITY);
        assert_eq!(
            queue.jobs.iter().filter(|job| job.important()).count(),
            FAILURE_RESERVE + 1
        );
        assert!(queue.dropped_logs > 0);
    }
    #[test]
    fn flush_makes_pending_rows_visible_and_shutdown_drains() {
        let db = Database::open_in_memory().unwrap();
        let (events, _) = broadcast::channel(16);
        let worker = DatabaseWorker::new(db.clone(), events).unwrap();
        for _ in 0..200 {
            worker.log(RequestLogEntry {
                proxy_id: None,
                target_host: "test",
                target_port: 80,
                success: true,
                response_time: Some(1),
                error_message: None,
                result_type: "tunnel_established",
            });
        }
        assert!(worker.flush(Duration::from_secs(2)));
        assert_eq!(db.traffic_logs(1, 25, None).unwrap().1, 200);
        assert_eq!(worker.stats()["droppedLogs"], json!(0));
        drop(worker);
    }
}
