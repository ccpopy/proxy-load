//! Enforce the wire-size limit before Hyper can accept a completed response head.
use super::probe_transport::MAX_HEADERS;
use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[derive(Default)]
struct HeaderBudget {
    consumed: usize,
    current: Vec<u8>,
    complete: bool,
}

impl HeaderBudget {
    fn observe(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self.complete {
            return Ok(());
        }
        for &byte in bytes {
            if self.consumed == MAX_HEADERS {
                return Err(io::ErrorKind::InvalidData.into());
            }
            self.consumed += 1;
            // Match the parser's leading-empty-line handling, but count those bytes too.
            if self.current.is_empty() && matches!(byte, b'\r' | b'\n') {
                continue;
            }
            self.current.push(byte);
            // Scan line endings incrementally rather than reparsing every partial read.
            if !self.current.ends_with(b"\n\n") && !self.current.ends_with(b"\n\r\n") {
                continue;
            }
            let mut headers = [httparse::EMPTY_HEADER; 128];
            let mut response = httparse::Response::new(&mut headers);
            if response
                .parse(&self.current)
                .map_err(|_| io::ErrorKind::InvalidData)?
                .is_complete()
            {
                let status = response.code.ok_or(io::ErrorKind::InvalidData)?;
                if (100..200).contains(&status) && status != 101 {
                    self.current.clear();
                } else {
                    self.complete = true;
                    self.current.clear();
                    return Ok(()); // Coalesced body bytes do not belong to the header budget.
                }
            }
        }
        Ok(())
    }
}

pub(crate) struct HeaderLimitedIo<T> {
    inner: T,
    budget: HeaderBudget,
    rejected: bool,
}

impl<T> HeaderLimitedIo<T> {
    pub(crate) fn new(inner: T) -> Self {
        Self {
            inner,
            budget: HeaderBudget::default(),
            rejected: false,
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for HeaderLimitedIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if out.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if self.rejected {
            return Poll::Ready(Err(io::ErrorKind::InvalidData.into()));
        }
        if self.budget.complete {
            return Pin::new(&mut self.inner).poll_read(cx, out);
        }
        let mut scratch = [0; 8192];
        let limit = out.remaining().min(scratch.len());
        let mut read = ReadBuf::new(&mut scratch[..limit]);
        match Pin::new(&mut self.inner).poll_read(cx, &mut read) {
            Poll::Ready(Ok(())) => {
                if let Err(error) = self.budget.observe(read.filled()) {
                    self.rejected = true;
                    return Poll::Ready(Err(error));
                }
                out.put_slice(read.filled());
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for HeaderLimitedIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
