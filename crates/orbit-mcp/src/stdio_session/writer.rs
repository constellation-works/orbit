//! Complete-frame stdout writes and flushed-response accounting.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use rmcp::model::{ErrorData, ServerJsonRpcMessage};
use tokio::io::AsyncWrite;

use super::tracking;

/// Same bytes `ErrorData::invalid_request("Invalid request", None)`
/// serializes to. The write shares [`write_complete_stdout_frame`] with
/// service responses and runs to completion on this task, outside the
/// service `select!` that cancels `receive`.
pub(super) async fn write_stdio_invalid_request() -> bool {
    let message =
        ServerJsonRpcMessage::error(ErrorData::invalid_request("Invalid request", None), None);
    let Ok(mut line) = serde_json::to_vec(&message) else {
        return false;
    };
    line.push(b'\n');
    tracing::debug!(
        target: "orbit.mcp",
        "answering an undecodable stdio line before the transport reads it"
    );
    tokio::task::spawn_blocking(move || write_complete_stdout_frame(&line).is_ok())
        .await
        .unwrap_or(false)
}

/// Writes one complete stdout record while holding the process stdout lock.
///
/// Tokio's `Stdout::poll_write` copies at most 2 MiB and drops that lock
/// between chunks. A permitted image read is larger than that once its
/// base64 is carried as both an image block and `structuredContent`, so a
/// second writer can insert bytes inside the record. One `write_all` keeps
/// the lock until the record is finished.
fn write_complete_stdout_frame(frame: &[u8]) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    std::io::Write::write_all(&mut stdout, frame)?;
    std::io::Write::flush(&mut stdout)
}

/// Stdout half that emits each encoded frame with [`write_complete_stdout_frame`].
///
/// `poll_write` only buffers. While a frame write is in flight it accepts
/// nothing and returns [`Poll::Pending`], waking when that write finishes.
/// `poll_flush` returns ready only after every byte already accepted has
/// been emitted, including bytes buffered before a cancelled flush was
/// polled again. A successful flush therefore cannot retire a response
/// whose bytes are still sitting in `staged`.
pub(super) struct CompleteFrameStdout {
    staged: Vec<u8>,
    flush: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl CompleteFrameStdout {
    pub(super) fn new() -> Self {
        Self {
            staged: Vec::new(),
            flush: None,
        }
    }

    /// `true` when this poll finished writing every accepted byte. An idle
    /// flush is `Ok(false)` so a caller does not treat an earlier failure
    /// as answered.
    fn poll_flush_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<bool>> {
        let this = self.get_mut();
        poll_frame_flush(&mut this.staged, &mut this.flush, cx)
    }
}

/// Drives the in-flight frame write. `Ok(false)` means nothing was in flight.
fn poll_in_flight_frame(
    flush: &mut Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    cx: &mut Context<'_>,
) -> Poll<std::io::Result<bool>> {
    let polled = {
        let Some(handle) = flush.as_mut() else {
            return Poll::Ready(Ok(false));
        };
        std::future::Future::poll(std::pin::Pin::new(handle), cx)
    };
    match polled {
        Poll::Pending => Poll::Pending,
        Poll::Ready(result) => {
            *flush = None;
            match result {
                Ok(Ok(())) => Poll::Ready(Ok(true)),
                Ok(Err(error)) => Poll::Ready(Err(error)),
                Err(error) => Poll::Ready(Err(std::io::Error::other(error))),
            }
        }
    }
}

fn poll_frame_flush(
    staged: &mut Vec<u8>,
    flush: &mut Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    cx: &mut Context<'_>,
) -> Poll<std::io::Result<bool>> {
    let mut wrote = false;
    loop {
        match poll_in_flight_frame(flush, cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(finished)) => wrote |= finished,
        }
        // Ready clears the handle. A flush still in flight made no progress.
        if flush.is_some() {
            return Poll::Ready(Err(std::io::Error::other(
                "stdio frame flush lost its in-flight write",
            )));
        }
        if staged.is_empty() {
            return Poll::Ready(Ok(wrote));
        }
        // Bytes accepted while the previous frame was in flight. Emit them
        // before reporting success, or a cancelled flush would retire a
        // response that has not been written.
        let frame = std::mem::take(staged);
        *flush = Some(tokio::task::spawn_blocking(move || {
            write_complete_stdout_frame(&frame)
        }));
    }
}

impl AsyncWrite for CompleteFrameStdout {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        // Do not accept bytes into `staged` while a frame is in flight.
        // Completing that write and then reporting flush success would
        // leave the new bytes unemitted. Drive the in-flight write so
        // this waker runs when it finishes; accept the buffer only after
        // that write has released the frame.
        if self.flush.is_some() {
            match poll_in_flight_frame(&mut self.flush, cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(_)) => {}
            }
        }
        self.staged.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.poll_flush_frame(cx) {
            Poll::Ready(Ok(_)) => Poll::Ready(Ok(())),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.poll_flush(cx)
    }
}

/// Passes the service's output through, retiring each answered request
/// once its response frame has been written.
///
/// A line is recorded only for the prefix [`CompleteFrameStdout`] actually
/// accepts. A `Pending` write accepts nothing, so the retry is the first
/// time those bytes are tracked.
pub(super) struct TrackingWriter {
    pub(super) inner: CompleteFrameStdout,
    pub(super) line: Vec<u8>,
    pub(super) answered: Vec<Vec<u8>>,
    pub(super) tracker: Arc<Mutex<tracking::Tracker>>,
}

impl AsyncWrite for TrackingWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let accepted = match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(accepted)) => accepted,
        };
        let this = &mut *self;
        for byte in buf.iter().take(accepted) {
            if *byte == b'\n' {
                this.answered.push(std::mem::take(&mut this.line));
            } else {
                this.line.push(*byte);
            }
        }
        Poll::Ready(Ok(accepted))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_flush_frame(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(false)) => Poll::Ready(Ok(())),
            Poll::Ready(Ok(true)) => {
                let mut tracker = tracking::lock_stdio_tracker(&this.tracker);
                for line in this.answered.drain(..) {
                    tracker.answered(&line);
                }
                Poll::Ready(Ok(()))
            }
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.poll_flush(cx)
    }
}
