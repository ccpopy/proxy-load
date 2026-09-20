//! Deterministic read boundaries: these checks do not depend on TCP packet coalescing.
use super::*;
use std::{
    io,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

struct ResponseIo {
    bytes: Vec<u8>,
    position: usize,
    chunk: usize,
    written: usize,
    flushed: bool,
    reader: Option<Waker>,
}

impl AsyncRead for ResponseIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if !self.flushed {
            self.reader = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let count = out
            .remaining()
            .min(self.chunk)
            .min(self.bytes.len() - self.position);
        out.put_slice(&self.bytes[self.position..self.position + count]);
        self.position += count;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for ResponseIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.written += bytes.len();
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.written > 0 {
            self.flushed = true;
            if let Some(reader) = self.reader.take() {
                reader.wake();
            }
        }
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn sized_head(size: usize, status: u16) -> Vec<u8> {
    let mut bytes = format!("HTTP/1.1 {status} Test\r\nContent-Length: 0\r\nX: ").into_bytes();
    bytes.resize(size - 4, b'x');
    bytes.extend_from_slice(b"\r\n\r\n");
    assert_eq!(bytes.len(), size);
    bytes
}

async fn receive(bytes: Vec<u8>, chunk: usize, absolute: bool) -> Result<u16, ProbeFailure> {
    let io = Box::new(ResponseIo {
        bytes,
        position: 0,
        chunk,
        written: 0,
        flushed: false,
        reader: None,
    });
    let url = Url::parse("http://probe.test/?secret=hidden").unwrap();
    let response = request_headers(
        io,
        &url,
        absolute,
        None,
        Instant::now() + Duration::from_secs(5),
        &mut ProbeDiagnostics::default(),
    )
    .await?;
    Ok(response.status().as_u16())
}

#[tokio::test]
async fn http_header_budget_is_strict_for_all_read_boundaries() {
    for absolute in [false, true] {
        for chunk in [1, 7, 1023, 4095, 8191, MAX_HEADERS + 64] {
            for size in [MAX_HEADERS - 1, MAX_HEADERS, MAX_HEADERS + 1] {
                let result = receive(sized_head(size, 200), chunk, absolute).await;
                assert_eq!(
                    result.is_ok(),
                    size <= MAX_HEADERS,
                    "size={size}, chunk={chunk}, absolute={absolute}: {result:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn http_header_budget_accumulates_informational_responses() {
    for chunk in [1, 8191, MAX_HEADERS + 64] {
        for excess in [0, 1] {
            let mut bytes = sized_head(MAX_HEADERS / 2, 103);
            bytes.extend(sized_head(MAX_HEADERS / 2 + excess, 200));
            let result = receive(bytes, chunk, true).await;
            assert_eq!(
                result.is_ok(),
                excess == 0,
                "excess={excess}, chunk={chunk}: {result:?}"
            );
        }
    }
}

#[tokio::test]
async fn http_header_budget_excludes_body_and_rejects_unterminated_headers() {
    for chunk in [1, 8191, MAX_HEADERS + 64] {
        let mut bytes = b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\n".to_vec();
        bytes.resize(bytes.len() + 100000, b'x');
        assert_eq!(receive(bytes, chunk, true).await.unwrap(), 200);
        let mut unfinished = sized_head(MAX_HEADERS, 200);
        unfinished.truncate(unfinished.len() - 2);
        assert!(receive(unfinished, chunk, true).await.is_err());
    }
}

#[tokio::test]
async fn http_header_budget_counts_empty_lines_and_mixed_informational_heads() {
    let prefix = b"\r\nHTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 103 Early Hints\nLink: </x>\n\n";
    for chunk in [1, 7, 8191] {
        for excess in [0, 1] {
            let mut bytes = prefix.to_vec();
            bytes.extend(sized_head(MAX_HEADERS - prefix.len() + excess, 200));
            let result = receive(bytes, chunk, true).await;
            assert_eq!(
                result.is_ok(),
                excess == 0,
                "excess={excess}, chunk={chunk}: {result:?}"
            );
        }
        assert_eq!(
            receive(
                b"HTTP/1.1 200 OK\nContent-Length: 0\n\n".to_vec(),
                chunk,
                true
            )
            .await
            .unwrap(),
            200
        );
    }
}
