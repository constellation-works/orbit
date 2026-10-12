//! Pipe workers and the bound on how long they outlive the child.
//!
//! One worker drains each output stream into its sink and one writes the
//! stdin payload. Each polls its pipe together with a [`StopWatch`], so the
//! supervisor can stop a worker whose pipe a process outside the child's
//! group still holds.

use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[cfg(unix)]
use super::super::pipe::set_nonblocking;
use super::{OutputSink, ProcessStopCause, SinkVerdict};

/// How often a worker without a wakeup descriptor rechecks the stop flag.
#[cfg(unix)]
const STOP_FLAG_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Supervisor half of the drain bound. Every worker holds a [`StopWatch`]
/// from [`Self::watch`]; [`Self::settle`] (or dropping this) stops them.
///
/// The stop is a flag every worker rechecks, and normally also travels over a
/// socket pair: dropping the supervisor's end makes the workers' ends
/// readable, which each worker observes in `poll(2)` alongside its own pipe.
/// When the pair or a worker's clone of it cannot be created (`EMFILE`), that
/// worker polls its pipe with a short timeout and rechecks the flag instead,
/// so the run still proceeds with the same bound. Every descriptor has exactly
/// one owner and is closed only by that owner.
pub(super) struct DrainStop {
    stop: StopSignal,
    #[cfg(unix)]
    watched: Option<UnixStream>,
    running_tx: SyncSender<()>,
    running_rx: Receiver<()>,
}

/// Raises the stop when dropped, on settling or on an early return.
struct StopSignal {
    stopped: Arc<AtomicBool>,
    #[cfg(unix)]
    _wakeup: Option<UnixStream>,
}

impl Drop for StopSignal {
    fn drop(&mut self) {
        // The wakeup end closes after this, so a worker woken by it always
        // sees the flag set too.
        self.stopped.store(true, Ordering::Release);
    }
}

/// Worker half of the drain bound; dropping it reports the worker finished.
pub(super) struct StopWatch {
    #[cfg(unix)]
    stopped: Arc<AtomicBool>,
    #[cfg(unix)]
    stop: Option<UnixStream>,
    // Never sent on: the supervisor sees every worker gone once all clones
    // are dropped.
    _running: SyncSender<()>,
}

impl DrainStop {
    pub(super) fn new() -> Self {
        // Nothing is ever sent; the channel only reports when the last
        // worker drops its sender.
        let (running_tx, running_rx) = mpsc::sync_channel(1);
        #[cfg(unix)]
        let (wakeup, watched) = match wakeup_pair() {
            Ok((wakeup, watched)) => (Some(wakeup), Some(watched)),
            Err(_) => (None, None),
        };
        Self {
            stop: StopSignal {
                stopped: Arc::new(AtomicBool::new(false)),
                #[cfg(unix)]
                _wakeup: wakeup,
            },
            #[cfg(unix)]
            watched,
            running_tx,
            running_rx,
        }
    }

    pub(super) fn watch(&self) -> StopWatch {
        StopWatch {
            #[cfg(unix)]
            stopped: Arc::clone(&self.stop.stopped),
            #[cfg(unix)]
            stop: self
                .watched
                .as_ref()
                .and_then(|watched| watched.try_clone().ok()),
            _running: self.running_tx.clone(),
        }
    }

    /// Wait up to `budget` for every worker to finish on its own, then stop
    /// the rest. Returns whether any worker had to be stopped.
    ///
    /// A stopped worker returns promptly, so joining it afterwards is
    /// bounded. Off Unix the workers use blocking I/O and cannot be stopped.
    pub(super) fn settle(self, budget: Duration) -> bool {
        let Self {
            stop,
            running_tx,
            running_rx,
            ..
        } = self;
        drop(running_tx);
        let finished = matches!(
            running_rx.recv_timeout(budget),
            Err(RecvTimeoutError::Disconnected)
        );
        drop(stop);
        !finished
    }
}

#[cfg(unix)]
fn wakeup_pair() -> io::Result<(UnixStream, UnixStream)> {
    if super::seam::no_wakeup_pair() {
        return Err(io::Error::from_raw_os_error(libc::EMFILE));
    }
    UnixStream::pair()
}

/// A pipe end the workers can wait on together with their [`StopWatch`].
#[cfg(unix)]
pub(super) trait PipeFd: AsFd {}
#[cfg(unix)]
impl<T: AsFd> PipeFd for T {}
#[cfg(not(unix))]
pub(super) trait PipeFd {}
#[cfg(not(unix))]
impl<T> PipeFd for T {}

/// Drain a child stream into `sink` until EOF, the sink closing or stopping
/// the run, or the drain bound stopping the worker. A stop is reported once on
/// `stop_tx`. The worker finishes the sink, so a relay closes and an echo
/// flushes before the supervisor joins it.
pub(super) fn spawn_sink_drain<R>(
    mut reader: R,
    mut sink: Box<dyn OutputSink>,
    stop_tx: SyncSender<ProcessStopCause>,
    stop: StopWatch,
) -> JoinHandle<Vec<u8>>
where
    R: Read + PipeFd + Send + 'static,
{
    thread::spawn(move || {
        pump(&mut reader, &stop, |bytes| match sink.push(bytes) {
            SinkVerdict::Continue => false,
            SinkVerdict::Close => true,
            SinkVerdict::Stop(cause) => {
                let _ = stop_tx.send(cause);
                true
            }
        });
        sink.finish()
    })
}

/// Feed `reader` into `sink` until EOF, a read error, `sink` returning `true`,
/// or `stop` firing. On stop, the bytes the pipe already buffers are still
/// read (without waiting for more), so output written before the stop is kept.
#[cfg(unix)]
fn pump<R: Read + AsFd>(reader: &mut R, stop: &StopWatch, mut sink: impl FnMut(&[u8]) -> bool) {
    if set_nonblocking(reader.as_fd()).is_err() {
        return;
    }
    let mut chunk = [0u8; 8192];
    loop {
        match wait_ready(reader.as_fd(), libc::POLLIN, stop) {
            Ok(Ready::Io) => {}
            Ok(Ready::Stopped) => {
                read_buffered(reader, &mut chunk, sink);
                return;
            }
            Err(_) => return,
        }
        match reader.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => {
                if sink(&chunk[..n]) {
                    return;
                }
            }
            Err(err) if is_retry(&err) => {}
            Err(_) => return,
        }
    }
}

#[cfg(not(unix))]
fn pump<R: Read>(reader: &mut R, _stop: &StopWatch, mut sink: impl FnMut(&[u8]) -> bool) {
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => {
                if sink(&chunk[..n]) {
                    return;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// Read what the pipe holds right now — no more, so a holder that keeps
/// writing cannot extend the stop.
#[cfg(unix)]
fn read_buffered<R: Read + AsFd>(
    reader: &mut R,
    chunk: &mut [u8],
    mut sink: impl FnMut(&[u8]) -> bool,
) {
    let mut remaining = buffered_len(reader.as_fd());
    while remaining > 0 {
        let want = remaining.min(chunk.len());
        match reader.read(&mut chunk[..want]) {
            Ok(0) => return,
            Ok(n) => {
                remaining = remaining.saturating_sub(n);
                if sink(&chunk[..n]) {
                    return;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

#[cfg(unix)]
enum Ready {
    Io,
    Stopped,
}

/// Block until `fd` reports `events` (or an error/hang-up the next I/O call
/// will surface) or the supervisor stops the worker; a stop wins a tie.
#[cfg(unix)]
fn wait_ready(fd: BorrowedFd<'_>, events: libc::c_short, stop: &StopWatch) -> io::Result<Ready> {
    let mut fds = [
        libc::pollfd {
            fd: fd.as_raw_fd(),
            events,
            revents: 0,
        },
        // `poll` ignores a negative descriptor.
        libc::pollfd {
            fd: stop.stop.as_ref().map_or(-1, AsRawFd::as_raw_fd),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // With a wakeup descriptor the worker can park until something happens;
    // without one it must wake periodically to observe the flag.
    let timeout_ms = if stop.stop.is_some() {
        -1
    } else {
        STOP_FLAG_POLL_INTERVAL.as_millis() as libc::c_int
    };
    loop {
        // Checked on every pass, so a continuously ready pipe cannot starve
        // the stop.
        if stop.stopped.load(Ordering::Acquire) {
            return Ok(Ready::Stopped);
        }
        // SAFETY: both descriptors are borrowed from handles that outlive
        // the call, and `fds` is a valid array of the length passed.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if rc > 0 {
            return Ok(if fds[1].revents != 0 {
                Ready::Stopped
            } else {
                Ready::Io
            });
        }
        if rc == 0 {
            continue;
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(unix)]
fn buffered_len(fd: BorrowedFd<'_>) -> usize {
    let mut available: libc::c_int = 0;
    // SAFETY: FIONREAD writes one `c_int` through a valid pointer.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), libc::FIONREAD, &mut available) };
    if rc < 0 {
        return 0;
    }
    usize::try_from(available).unwrap_or(0)
}

#[cfg(unix)]
fn is_retry(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

pub(super) fn spawn_stdin_write<W>(
    mut stdin: W,
    bytes: Vec<u8>,
    result_tx: SyncSender<std::io::Result<()>>,
    stop: StopWatch,
) -> JoinHandle<()>
where
    W: Write + PipeFd + Send + 'static,
{
    thread::spawn(move || {
        let result = write_until_stopped(&mut stdin, &bytes, &stop);
        drop(stdin);
        let _ = result_tx.send(result);
    })
}

/// Write all of `bytes` unless the supervisor stops the writer first. A stop
/// means the child is gone and whatever still holds the pipe never read the
/// rest, which the supervisor treats exactly like the child closing its end:
/// a broken pipe.
#[cfg(unix)]
fn write_until_stopped<W: Write + AsFd>(
    writer: &mut W,
    mut bytes: &[u8],
    stop: &StopWatch,
) -> io::Result<()> {
    set_nonblocking(writer.as_fd())?;
    while !bytes.is_empty() {
        if let Ready::Stopped = wait_ready(writer.as_fd(), libc::POLLOUT, stop)? {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "stdin abandoned after the process exited",
            ));
        }
        match writer.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(err) if is_retry(&err) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn write_until_stopped<W: Write>(
    writer: &mut W,
    bytes: &[u8],
    _stop: &StopWatch,
) -> io::Result<()> {
    writer.write_all(bytes)
}
