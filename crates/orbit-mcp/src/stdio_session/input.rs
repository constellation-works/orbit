//! Plain-stdin line pumping and delivery of forwarded lines to the service.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};
use tokio::sync::mpsc;

use super::{MAX_STDIO_LINE, READ_CHUNK, tracking, writer};

/// Read complete lines from a non-pollable stdin and forward them to the
/// service, answering a rejected line here instead of forwarding it. This
/// mirrors the unix pump's `forward_lines`: rmcp's `receive` clears such a
/// line and then sends its error from a `select!` branch that a ready
/// response task cancels, so the client would never see the rejection.
pub(super) async fn pump_plain_lines<R>(
    mut reader: R,
    lines: mpsc::Sender<Vec<u8>>,
    undelivered: Arc<AtomicUsize>,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    use tokio::io::AsyncReadExt as _;
    let mut buffer = Vec::new();
    let mut chunk = vec![0u8; READ_CHUNK];
    loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) => {
                if !buffer.is_empty() {
                    let line = std::mem::take(&mut buffer);
                    undelivered.fetch_add(1, Ordering::SeqCst);
                    let _ = lines.send(line).await;
                }
                return;
            }
            Ok(read) => read,
            Err(_) => return,
        };
        let mut remaining = &chunk[..read];
        while !remaining.is_empty() {
            let end = remaining.iter().position(|byte| *byte == b'\n');
            let take = end.map_or(remaining.len(), |index| index + 1);
            if buffer.len() + take > MAX_STDIO_LINE {
                tracing::warn!(target: "orbit.mcp", "closing stdio session: input line exceeds the transport limit");
                return;
            }
            buffer.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            if end.is_some() && !forward_plain_lines(&mut buffer, &lines, &undelivered).await {
                return;
            }
        }
    }
}

/// Forward every complete line in `buffer`, leaving a partial one. False
/// when the service stopped listening or the rejection could not be written.
async fn forward_plain_lines(
    buffer: &mut Vec<u8>,
    lines: &mpsc::Sender<Vec<u8>>,
    undelivered: &AtomicUsize,
) -> bool {
    while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
        let line: Vec<u8> = buffer.drain(..=end).collect();
        if tracking::rmcp_receive_would_reject(&line) {
            if !writer::write_stdio_invalid_request().await {
                return false;
            }
            continue;
        }
        undelivered.fetch_add(1, Ordering::SeqCst);
        if lines.send(line).await.is_err() {
            return false;
        }
    }
    true
}

/// Delivers forwarded lines to the service, counting each one consumed.
pub(super) struct LineReader {
    pub(super) lines: mpsc::Receiver<Vec<u8>>,
    pub(super) current: Vec<u8>,
    pub(super) position: usize,
    pub(super) undelivered: Arc<AtomicUsize>,
}

impl AsyncRead for LineReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        if this.position >= this.current.len() {
            match this.lines.poll_recv(cx) {
                Poll::Ready(Some(line)) => {
                    this.current = line;
                    this.position = 0;
                }
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
        let remaining = &this.current[this.position..];
        let take = remaining.len().min(buf.remaining());
        buf.put_slice(&remaining[..take]);
        this.position += take;
        if this.position >= this.current.len() {
            this.undelivered.fetch_sub(1, Ordering::SeqCst);
        }
        Poll::Ready(Ok(()))
    }
}
