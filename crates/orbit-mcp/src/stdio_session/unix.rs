//! Reading fd 0 without buffering past one partial line.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::unix::AsyncFd;
use tokio::sync::{mpsc, oneshot};

use super::{MAX_CARRYOVER, MAX_STDIO_LINE, READ_CHUNK, tracking, writer};

pub(super) struct StdinFd(OwnedFd);

impl AsRawFd for StdinFd {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl StdinFd {
    fn duplicate() -> std::io::Result<Self> {
        let fd = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a successful F_DUPFD_CLOEXEC returns a new descriptor
        // owned by the caller.
        Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
    }
}

pub(super) struct NonBlockingStdin(AsyncFd<StdinFd>);

impl NonBlockingStdin {
    /// `None` when stdin cannot be polled (a regular file, `/dev/null`).
    pub(super) fn new() -> Option<Self> {
        set_nonblocking(true)?;
        let fd = match StdinFd::duplicate() {
            Ok(fd) => fd,
            Err(_) => {
                set_nonblocking(false);
                return None;
            }
        };
        // SAFETY: `fd` owns its duplicated descriptor. This module keeps
        // it open and unchanged for the lifetime of the registration.
        match unsafe { AsyncFd::register(fd) } {
            Ok(fd) => Some(Self(fd)),
            Err(_) => {
                set_nonblocking(false);
                None
            }
        }
    }
}

/// Whether [`NonBlockingStdin::new`] would succeed, asked before any
/// runtime exists: register stdin with a throwaway reactor the same way.
pub(super) fn stdin_is_pollable() -> bool {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
    else {
        return false;
    };
    let _entered = runtime.enter();
    let Ok(fd) = StdinFd::duplicate() else {
        return false;
    };
    // SAFETY: `fd` owns its duplicated descriptor and remains open and
    // unchanged until the temporary registration is dropped below.
    unsafe { AsyncFd::register(fd) }.is_ok()
}

fn set_nonblocking(enabled: bool) -> Option<()> {
    // SAFETY: fcntl on the process's own stdin descriptor with integer
    // flag arguments; no memory is passed.
    let flags = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL) };
    if flags < 0 {
        return None;
    }
    let flags = if enabled {
        flags | libc::O_NONBLOCK
    } else {
        flags & !libc::O_NONBLOCK
    };
    // SAFETY: as above.
    (unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, flags) } >= 0).then_some(())
}

/// Hand stdin to the next image the way this one received it.
pub(super) fn restore_blocking_stdin() {
    let _ = set_nonblocking(false);
}

pub(super) enum PumpOutcome {
    /// Stopped on request, holding the partial line read so far and the
    /// sender that keeps the service's input open.
    Stopped {
        carryover: Vec<u8>,
        input: mpsc::Sender<Vec<u8>>,
    },
    /// The client closed stdin.
    Closed,
}

pub(super) enum PumpStop {
    /// Stop regardless of the partial line, for closure or yielding.
    Always,
    /// Stop only if the partial line fits in the resumed session state.
    HandOver(oneshot::Sender<bool>),
}

/// Forward complete lines from stdin until stopped or closed.
pub(super) async fn pump_stdio_lines(
    stdin: NonBlockingStdin,
    mut buffer: Vec<u8>,
    lines: mpsc::Sender<Vec<u8>>,
    tracker: Arc<Mutex<tracking::Tracker>>,
    undelivered: Arc<AtomicUsize>,
    mut stopped: mpsc::Receiver<PumpStop>,
) -> std::io::Result<PumpOutcome> {
    // Read the same owned duplicate that Tokio registered for readiness.
    let fd = stdin.0.get_ref().as_raw_fd();
    if !forward_lines(&mut buffer, &lines, &tracker, &undelivered).await {
        return Ok(PumpOutcome::Closed);
    }
    let mut chunk = vec![0u8; READ_CHUNK];
    loop {
        let mut ready = tokio::select! {
            biased;
            request = stopped.recv() => {
                if let Some(PumpStop::HandOver(accepted)) = request {
                    if buffer.len() > MAX_CARRYOVER {
                        let _ = accepted.send(false);
                        continue;
                    }
                    let _ = accepted.send(true);
                }
                return Ok(PumpOutcome::Stopped {
                    carryover: buffer,
                    input: lines,
                });
            }
            ready = stdin.0.readable() => ready?,
        };
        let read = ready.try_io(|_| {
            // SAFETY: `stdin` owns `fd` for the full pump and this buffer
            // is writable for its length.
            let read = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
            if read < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(read as usize)
            }
        });
        let read = match read {
            Ok(Ok(read)) => read,
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Ok(Err(error)) => return Err(error),
            Err(_would_block) => continue,
        };
        if read == 0 {
            if !buffer.is_empty() {
                let line = std::mem::take(&mut buffer);
                undelivered.fetch_add(1, Ordering::SeqCst);
                let _ = lines.send(line).await;
            }
            return Ok(PumpOutcome::Closed);
        }
        // Check each line before appending or parsing it. A read may
        // contain multiple lines, so a chunk-wide length check would
        // incorrectly reject several small requests.
        let mut remaining = &chunk[..read];
        while !remaining.is_empty() {
            let end = remaining.iter().position(|byte| *byte == b'\n');
            let take = end.map_or(remaining.len(), |index| index + 1);
            if buffer.len() + take > MAX_STDIO_LINE {
                tracing::warn!(target: "orbit.mcp", "closing stdio session: input line exceeds the transport limit");
                return Ok(PumpOutcome::Closed);
            }
            buffer.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            if end.is_some() && !forward_lines(&mut buffer, &lines, &tracker, &undelivered).await {
                return Ok(PumpOutcome::Closed);
            }
        }
    }
}

/// Forward every complete line in `buffer`, leaving a partial one. False
/// when the service stopped listening.
async fn forward_lines(
    buffer: &mut Vec<u8>,
    lines: &mpsc::Sender<Vec<u8>>,
    tracker: &Mutex<tracking::Tracker>,
    undelivered: &AtomicUsize,
) -> bool {
    while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
        let line: Vec<u8> = buffer.drain(..=end).collect();
        // Answer here, before rmcp reads the line. `receive` clears the
        // line and then sends this error from a `select!` branch that a
        // ready response task cancels. The frame uses the service's stdout
        // writer, so it stays a whole record beside a large response. Do
        // not count the line: the service never reads it, and a pending
        // count would stall handover.
        if tracking::rmcp_receive_would_reject(&line) {
            if !writer::write_stdio_invalid_request().await {
                return false;
            }
            continue;
        }
        // Record the request before the service can answer it.
        tracking::lock_stdio_tracker(tracker).forwarded(&line);
        undelivered.fetch_add(1, Ordering::SeqCst);
        if lines.send(line).await.is_err() {
            return false;
        }
    }
    true
}
