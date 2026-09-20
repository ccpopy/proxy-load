use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::oneshot;
const MAX_HEADER: usize = 64 * 1024;
#[derive(Default)]
pub(super) struct TransferCounters {
    pub read: AtomicU64,
    pub written: AtomicU64,
}

/// Observes reads without withholding bytes, buffering bodies or blocking uploads.
pub(super) struct HttpObserver<T> {
    inner: T,
    prefix: Vec<u8>,
    observed: usize,
    sender: Option<oneshot::Sender<u16>>,
    counters: Arc<TransferCounters>,
}
impl<T> HttpObserver<T> {
    #[cfg(test)]
    pub(super) fn new(inner: T, sender: oneshot::Sender<u16>) -> Self {
        Self::with_counters(inner, Some(sender), Arc::new(TransferCounters::default()))
    }
    pub(super) fn with_counters(
        inner: T,
        sender: Option<oneshot::Sender<u16>>,
        counters: Arc<TransferCounters>,
    ) -> Self {
        Self {
            inner,
            prefix: Vec::new(),
            observed: 0,
            sender,
            counters,
        }
    }
    fn observe(&mut self, bytes: &[u8]) {
        if self.sender.is_none() {
            return;
        }
        for byte in bytes {
            self.observed += 1;
            if self.observed > MAX_HEADER {
                self.sender.take();
                self.prefix.clear();
                return;
            }
            self.prefix.push(*byte);
            if self.prefix.ends_with(b"\r\n\r\n") {
                let status = std::str::from_utf8(&self.prefix).ok().and_then(|header| {
                    let mut fields = header.lines().next()?.split_whitespace();
                    if !matches!(fields.next()?, "HTTP/1.0" | "HTTP/1.1") {
                        return None;
                    }
                    fields
                        .next()?
                        .parse::<u16>()
                        .ok()
                        .filter(|status| (100..=599).contains(status))
                });
                self.prefix.clear();
                match status {
                    Some(100..=199) if status != Some(101) => continue,
                    Some(status) => {
                        if let Some(sender) = self.sender.take() {
                            let _ = sender.send(status);
                        }
                    }
                    None => {
                        self.sender.take();
                    }
                }
                return;
            }
        }
    }
}
impl<T: AsyncRead + Unpin> AsyncRead for HttpObserver<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        let result = Pin::new(&mut this.inner).poll_read(cx, buf);
        if matches!(result, Poll::Ready(Ok(()))) {
            this.counters
                .read
                .fetch_add((buf.filled().len() - before) as u64, Ordering::Relaxed);
            this.observe(&buf.filled()[before..]);
        }
        result
    }
}
impl<T: AsyncWrite + Unpin> AsyncWrite for HttpObserver<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_write(cx, bytes);
        if let Poll::Ready(Ok(count)) = result {
            this.counters
                .written
                .fetch_add(count as u64, Ordering::Relaxed);
        }
        result
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn final_status_after_continue_is_observed_at_every_fragment() {
        for final_status in [101, 200, 407, 503] {
            let response = format!(
                "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 {final_status} Result\r\nX: yes\r\n\r\nbody"
            );
            for split in 0..response.len() {
                let (tx, rx) = oneshot::channel();
                let mut observer = HttpObserver::new((), tx);
                observer.observe(&response.as_bytes()[..split]);
                observer.observe(&response.as_bytes()[split..]);
                assert_eq!(rx.await.unwrap(), final_status);
                assert!(observer.prefix.len() <= MAX_HEADER);
            }
        }
    }
    #[test]
    fn endless_informational_headers_are_bounded() {
        let (tx, mut rx) = oneshot::channel();
        let mut observer = HttpObserver::new((), tx);
        for _ in 0..10000 {
            observer.observe(b"HTTP/1.1 100 Continue\r\n\r\n");
        }
        assert!(observer.sender.is_none());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn upload_half_close_keeps_streaming_download_alive() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut client, mut incoming) = tokio::io::duplex(16);
        let (upstream, mut server) = tokio::io::duplex(16);
        let (tx, rx) = oneshot::channel();
        let copy = tokio::spawn(async move {
            let mut observed = HttpObserver::new(upstream, tx);
            tokio::io::copy_bidirectional(&mut incoming, &mut observed)
                .await
                .unwrap()
        });
        let remote = tokio::spawn(async move {
            let mut upload = Vec::new();
            server.read_to_end(&mut upload).await.unwrap();
            assert_eq!(upload, b"upload");
            server.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
            server.write_all(&vec![b'x'; 10000]).await.unwrap();
            server.shutdown().await.unwrap();
        });
        client.write_all(b"upload").await.unwrap();
        client.shutdown().await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert!(response.ends_with(&vec![b'x'; 10000]));
        assert_eq!(rx.await.unwrap(), 200);
        assert_eq!(copy.await.unwrap(), (6, response.len() as u64));
        remote.await.unwrap();
    }
}
