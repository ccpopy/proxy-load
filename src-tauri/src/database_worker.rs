use crate::{
    database::{Database, RequestLogEntry},
    models::{ProxyRecord, ServerEvent},
    state::now_millis,
};
use anyhow::Result;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
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
impl OwnedLog {
    fn important(&self) -> bool {
        match self.kind.as_str() {
            "forwarded_unverified" | "tunnel_established" | "transfer_finished" => false,
            "proxy_exhausted" | "tunnel_setup_error" | "transfer_error" => true,
            "upstream_response_observed" => !self.success,
            _ => !self.success,
        }
    }
}
struct StatusChange {
    proxy: ProxyRecord,
    generation: u64,
    revision: u64,
    status: String,
    time: Option<i64>,
}
struct ClearRequest {
    // Number of queued logs preceding this barrier (in-flight batch also precedes it).
    remaining: usize,
    reply: std::sync::mpsc::SyncSender<std::result::Result<i64, String>>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
}
#[derive(Default)]
struct Queue {
    jobs: VecDeque<OwnedLog>,
    statuses: HashMap<i64, StatusChange>,
    clear: Option<ClearRequest>,
    enqueued: VecDeque<std::time::Instant>,
    stopped: bool,
    working: bool,
    dropped_logs: u64,
    dropped_status: u64,
    coalesced_status: u64,
    retried_status: u64,
    #[cfg(test)]
    paused: bool,
    #[cfg(test)]
    fail_status_writes: usize,
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
                    while (queue.jobs.is_empty()
                        && queue.statuses.is_empty()
                        && queue.clear.is_none()
                        && !queue.stopped)
                        || queue.paused_for_test()
                    {
                        queue = state.wake.wait(queue).unwrap_or_else(|e| e.into_inner());
                    }
                    if queue.jobs.is_empty()
                        && queue.statuses.is_empty()
                        && queue.clear.is_none()
                        && queue.stopped
                    {
                        break;
                    }
                    if queue
                        .clear
                        .as_ref()
                        .is_some_and(|clear| clear.remaining == 0)
                    {
                        let clear = queue.clear.take().unwrap();
                        queue.working = true;
                        drop(queue);
                        if !clear.cancelled.load(std::sync::atomic::Ordering::Acquire) {
                            let result = writer
                                .clear_traffic_logs()
                                .map_err(|error| error.to_string());
                            let _ = clear.reply.send(result);
                        }
                        let mut queue = state.queue.lock().unwrap_or_else(|e| e.into_inner());
                        queue.working = false;
                        state.drained.notify_all();
                        continue;
                    }
                    // Coalesce bursts, bounded by 100 ms; exit flush skips the delay.
                    if queue.jobs.len() < BATCH_SIZE && !queue.stopped {
                        queue = state
                            .wake
                            .wait_timeout(queue, Duration::from_millis(100))
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                    }
                    if queue.paused_for_test() {
                        continue;
                    }
                    let count = queue.jobs.len().min(BATCH_SIZE).min(
                        queue
                            .clear
                            .as_ref()
                            .map_or(usize::MAX, |clear| clear.remaining),
                    );
                    if let Some(clear) = &mut queue.clear {
                        clear.remaining -= count;
                    }
                    let batch = queue.jobs.drain(..count).collect::<Vec<_>>();
                    let ids = queue
                        .statuses
                        .keys()
                        .take(BATCH_SIZE)
                        .copied()
                        .collect::<Vec<_>>();
                    let statuses = ids
                        .into_iter()
                        .filter_map(|id| queue.statuses.remove(&id))
                        .collect::<Vec<_>>();
                    #[cfg(test)]
                    let fail_status_write = if !statuses.is_empty() && queue.fail_status_writes > 0
                    {
                        queue.fail_status_writes -= 1;
                        true
                    } else {
                        false
                    };
                    for enqueued in queue.enqueued.drain(..count).collect::<Vec<_>>() {
                        queue.max_queue_wait_ms = queue
                            .max_queue_wait_ms
                            .max(enqueued.elapsed().as_millis() as u64);
                    }
                    queue.working = true;
                    drop(queue);
                    let start = std::time::Instant::now();
                    let logs = batch;
                    let mut errors = 0;
                    let had_statuses = !statuses.is_empty();
                    let mut retry = Vec::new();
                    for change in statuses {
                        let persist = || {
                            // Configuration and health publish the same routing
                            // snapshot; serialize both on the main writer connection.
                            db.update_passive_status(
                                &change.proxy,
                                change.generation,
                                change.revision,
                                &change.status,
                                change.time,
                            )
                        };
                        #[cfg(not(test))]
                        let result = persist();
                        #[cfg(test)]
                        let result = if fail_status_write {
                            Err(anyhow::anyhow!("injected database busy"))
                        } else {
                            persist()
                        };
                        if let Err(error) = result {
                            eprintln!("后台状态持久化失败: {error:#}");
                            errors += 1;
                            retry.push(change);
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
                    for change in retry {
                        if queue.stopped {
                            queue.dropped_status += 1;
                        } else if db.routing_snapshot().node_generations.get(&change.proxy.id)
                            == Some(&change.generation)
                            && db.status_revision(change.proxy.id) == change.revision
                            && !queue.statuses.contains_key(&change.proxy.id)
                        {
                            queue.statuses.insert(change.proxy.id, change);
                            queue.retried_status += 1;
                        }
                    }
                    queue.errors += errors;
                    queue.dropped_logs += logs.len() as u64 - written;
                    queue.written += written;
                    queue.max_wait_ms = queue.max_wait_ms.max(start.elapsed().as_millis() as u64);
                    queue.working = false;
                    state.drained.notify_all();
                    drop(queue);
                    if ((written != 0 || had_statuses)
                        && last_event.elapsed() >= Duration::from_millis(250))
                        || errors != 0
                    {
                        last_event = std::time::Instant::now();
                        let _ = events.send(ServerEvent {
                            event_type: "request_logged".into(),
                            data: Value::Null,
                            timestamp: now_millis(),
                        });
                    }
                    if errors != 0 {
                        // Retry latest state without spinning when SQLite remains unavailable.
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            })?;
        Ok(Self(Arc::new(Worker {
            db: status_db,
            shared,
            join: Mutex::new(Some(join)),
        })))
    }
    fn enqueue(&self, job: OwnedLog) -> bool {
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
                    if let Some(clear) = &mut queue.clear {
                        if index < clear.remaining {
                            clear.remaining -= 1;
                        }
                    }
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
        self.enqueue(entry.into());
    }
    pub fn status(
        &self,
        proxy: ProxyRecord,
        generation: u64,
        status: &str,
        time: Option<i64>,
    ) -> bool {
        let mut queue = self
            .0
            .shared
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let snapshot = self.0.db.routing_snapshot();
        if queue.stopped || snapshot.node_generations.get(&proxy.id) != Some(&generation) {
            queue.dropped_status += 1;
            return false;
        }
        // Bounded by configured nodes, independent of log pressure; never reserve
        // a new revision for a state that cannot be retained.
        queue
            .statuses
            .retain(|id, change| snapshot.node_generations.get(id) == Some(&change.generation));
        let revision = self.0.db.reserve_status_revision(proxy.id);
        let previous = queue.statuses.insert(
            proxy.id,
            StatusChange {
                proxy,
                generation,
                revision,
                status: status.into(),
                time,
            },
        );
        if previous.is_some() {
            queue.coalesced_status += 1;
        }
        self.0.shared.wake.notify_one();
        true
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
            .wait_timeout_while(queue, budget, |q| {
                !q.jobs.is_empty() || !q.statuses.is_empty() || q.clear.is_some() || q.working
            })
            .unwrap_or_else(|e| e.into_inner());
        queue.jobs.is_empty()
            && queue.statuses.is_empty()
            && queue.clear.is_none()
            && !queue.working
    }
    pub fn clear_logs(&self, budget: Duration) -> Result<i64> {
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut queue = self
                .0
                .shared
                .queue
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            anyhow::ensure!(
                !queue.stopped && queue.clear.is_none(),
                "日志写入已停止或另一次清空仍在排队"
            );
            queue.clear = Some(ClearRequest {
                remaining: queue.jobs.len(),
                reply,
                cancelled: cancelled.clone(),
            });
        }
        self.0.shared.wake.notify_one();
        match result.recv_timeout(budget) {
            Ok(result) => result.map_err(anyhow::Error::msg),
            Err(error) => {
                cancelled.store(true, std::sync::atomic::Ordering::Release);
                let mut queue = self
                    .0
                    .shared
                    .queue
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if queue
                    .clear
                    .as_ref()
                    .is_some_and(|clear| Arc::ptr_eq(&clear.cancelled, &cancelled))
                {
                    queue.clear = None;
                    anyhow::bail!("清空日志等待超时，尚未开始的清空已取消: {error}");
                }
                anyhow::bail!("清空日志等待超时，已开始的操作可能仍在执行: {error}")
            }
        }
    }
    pub fn seal_and_flush(&self, budget: Duration) -> bool {
        let errors = {
            let mut queue = self
                .0
                .shared
                .queue
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            queue.stopped = true;
            queue.errors
        };
        let drained = self.flush(budget);
        drained
            && self
                .0
                .shared
                .queue
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .errors
                == errors
    }
    pub fn stats(&self) -> Value {
        let queue = self
            .0
            .shared
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        json!({"queueLength": queue.jobs.len(), "statusQueueLength": queue.statuses.len(), "droppedStatus": queue.dropped_status, "coalescedStatus": queue.coalesced_status, "retriedStatus": queue.retried_status, "peakQueueLength": queue.peak_queue_length, "maxQueueWaitMs": queue.max_queue_wait_ms, "capacity": CAPACITY, "droppedLogs": queue.dropped_logs, "databaseErrors": queue.errors, "writtenLogs": queue.written, "maxBatchDurationMs": queue.max_wait_ms})
    }

    #[cfg(test)]
    pub(crate) fn pause_for_test(&self) -> TestPause<'_> {
        let mut queue = self.0.shared.queue.lock().unwrap();
        assert!(!queue.working);
        queue.paused = true;
        TestPause(self)
    }
}

impl Queue {
    fn paused_for_test(&self) -> bool {
        #[cfg(test)]
        {
            self.paused
        }
        #[cfg(not(test))]
        {
            false
        }
    }
}

#[cfg(test)]
pub(crate) struct TestPause<'a>(&'a DatabaseWorker);
#[cfg(test)]
impl Drop for TestPause<'_> {
    fn drop(&mut self) {
        self.0 .0.shared.queue.lock().unwrap().paused = false;
        self.0 .0.shared.wake.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_proxy(db: &Database) -> ProxyRecord {
        db.create_proxy(crate::models::ProxyInput {
            name: "state".into(),
            proxy_type: "http".into(),
            host: "127.0.0.1".into(),
            port: 9001,
            username: None,
            password: None,
            enabled: Some(1),
            test_url: None,
            test_timeout: None,
            skip_cert_verify: None,
        })
        .unwrap()
    }

    #[test]
    fn latest_state_retries_database_errors_and_rejects_old_revisions() {
        let db = Database::open_in_memory().unwrap();
        let proxy = test_proxy(&db);
        let generation = db.routing_snapshot().node_generations[&proxy.id];
        let (events, _) = broadcast::channel(16);
        let worker = DatabaseWorker::new(db.clone(), events).unwrap();
        let pause = worker.pause_for_test();
        worker.0.shared.queue.lock().unwrap().fail_status_writes = 1;
        assert!(worker.status(proxy.clone(), generation, "inactive", None));
        let old_revision = db.status_revision(proxy.id);
        assert!(worker.status(proxy.clone(), generation, "active", Some(5)));
        drop(pause);
        assert!(worker.flush(Duration::from_secs(3)));
        assert_eq!(worker.stats()["retriedStatus"], 1);
        assert_eq!(worker.stats()["databaseErrors"], 1);
        db.update_passive_status(&proxy, generation, old_revision, "inactive", None)
            .unwrap();
        assert_eq!(
            db.get_proxy(proxy.id).unwrap().unwrap().status.as_deref(),
            Some("active")
        );
        let revision = db.status_revision(proxy.id);
        assert!(worker.seal_and_flush(Duration::from_secs(1)));
        assert!(!worker.status(proxy.clone(), generation, "inactive", None));
        assert_eq!(
            db.status_revision(proxy.id),
            revision,
            "rejected status must not invalidate accepted state"
        );
        assert_eq!(worker.stats()["droppedStatus"], 1);
        assert_eq!(worker.stats()["droppedLogs"], 0);
    }

    #[test]
    fn unverified_forwarding_does_not_use_failure_reserve() {
        let worker = DatabaseWorker(Arc::new(Worker {
            db: Database::open_in_memory().unwrap(),
            shared: Arc::new(Shared {
                queue: Mutex::new(Queue::default()),
                wake: Condvar::new(),
                drained: Condvar::new(),
            }),
            join: Mutex::new(None),
        }));
        for _ in 0..CAPACITY {
            worker.log(RequestLogEntry {
                proxy_id: None,
                target_host: "forward",
                target_port: 80,
                success: false,
                response_time: None,
                error_message: None,
                result_type: "forwarded_unverified",
            });
        }
        assert_eq!(worker.stats()["queueLength"], CAPACITY - FAILURE_RESERVE);
    }

    fn log_named(worker: &DatabaseWorker, host: &str) {
        worker.log(RequestLogEntry {
            proxy_id: None,
            target_host: host,
            target_port: 80,
            success: true,
            response_time: Some(1),
            error_message: None,
            result_type: "tunnel_established",
        });
    }

    #[test]
    fn clear_barrier_removes_its_prefix_but_keeps_later_live_producers() {
        let db = Database::open_in_memory().unwrap();
        let (events, _) = broadcast::channel(16);
        let worker = DatabaseWorker::new(db.clone(), events).unwrap();
        let pause = worker.pause_for_test();
        for _ in 0..200 {
            log_named(&worker, "before-clear");
        }
        let clearing = thread::spawn({
            let worker = worker.clone();
            move || worker.clear_logs(Duration::from_secs(3))
        });
        let start = std::time::Instant::now();
        while worker.0.shared.queue.lock().unwrap().clear.is_none() {
            assert!(start.elapsed() < Duration::from_secs(1));
            thread::sleep(Duration::from_millis(1));
        }
        for _ in 0..20 {
            log_named(&worker, "after-clear");
        }
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let producer = thread::spawn({
            let worker = worker.clone();
            let done = done.clone();
            move || {
                while !done.load(std::sync::atomic::Ordering::Relaxed) {
                    log_named(&worker, "after-clear");
                    thread::sleep(Duration::from_millis(1));
                }
            }
        });
        drop(pause);
        assert_eq!(clearing.join().unwrap().unwrap(), 200);
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        producer.join().unwrap();
        assert!(worker.flush(Duration::from_secs(3)));
        let (rows, count) = db.traffic_logs(1, 200, None).unwrap();
        assert!(count >= 20);
        assert_eq!(db.traffic_logs(1, 25, Some("before-clear")).unwrap().1, 0);
        assert!(rows
            .iter()
            .all(|row| row.target_host.as_deref() == Some("after-clear")));
    }

    #[test]
    fn queued_clear_timeout_cancels_without_a_late_delete() {
        let db = Database::open_in_memory().unwrap();
        let (events, _) = broadcast::channel(16);
        let worker = DatabaseWorker::new(db.clone(), events).unwrap();
        let pause = worker.pause_for_test();
        log_named(&worker, "keep");
        assert!(worker.clear_logs(Duration::from_millis(10)).is_err());
        assert!(worker.0.shared.queue.lock().unwrap().clear.is_none());
        drop(pause);
        assert!(worker.flush(Duration::from_secs(1)));
        assert_eq!(db.traffic_logs(1, 25, None).unwrap().1, 1);
    }

    #[test]
    fn shutdown_does_not_report_success_when_final_status_write_fails() {
        let db = Database::open_in_memory().unwrap();
        let proxy = test_proxy(&db);
        let generation = db.routing_snapshot().node_generations[&proxy.id];
        let (events, _) = broadcast::channel(16);
        let worker = DatabaseWorker::new(db.clone(), events).unwrap();
        let pause = worker.pause_for_test();
        worker.0.shared.queue.lock().unwrap().fail_status_writes = 1;
        worker.status(proxy, generation, "active", Some(1));
        let stopping = thread::spawn({
            let worker = worker.clone();
            move || worker.seal_and_flush(Duration::from_secs(2))
        });
        let started = std::time::Instant::now();
        while !worker.0.shared.queue.lock().unwrap().stopped {
            assert!(started.elapsed() < Duration::from_secs(1));
            thread::sleep(Duration::from_millis(1));
        }
        drop(pause);
        assert!(!stopping.join().unwrap());
        assert_eq!(worker.stats()["droppedStatus"], 1);
        assert_eq!(worker.stats()["databaseErrors"], 1);
    }
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
    #[test]
    fn shutdown_barrier_rejects_future_producers_and_drains_accepted_logs() {
        let db = Database::open_in_memory().unwrap();
        let (events, _) = broadcast::channel(16);
        let worker = DatabaseWorker::new(db.clone(), events).unwrap();
        let log = || RequestLogEntry {
            proxy_id: None,
            target_host: "before-exit",
            target_port: 80,
            success: true,
            response_time: Some(1),
            error_message: None,
            result_type: "tunnel_established",
        };
        worker.log(log());
        assert!(worker.seal_and_flush(Duration::from_secs(2)));
        worker.log(log());
        assert!(worker.flush(Duration::from_secs(2)));
        assert_eq!(db.traffic_logs(1, 25, None).unwrap().1, 1);
        assert_eq!(worker.stats()["droppedLogs"], 1);
    }
}
