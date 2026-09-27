use std::io::{self, PipeWriter, Read, Write};
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use orbit_common::process::output_capture::{BoundedOutputCapture, capture_limit_from_env};
use orbit_common::security::redaction::redact_sensitive_env_text;

pub(super) const ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_ENV: &str =
    "ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_BYTES";
pub(super) const DEFAULT_OUTPUT_CAPTURE_LIMIT_BYTES: usize = 1024 * 1024;

pub(super) fn output_capture_limit() -> usize {
    capture_limit_from_env(
        ORBIT_EXEC_OUTPUT_CAPTURE_LIMIT_ENV,
        DEFAULT_OUTPUT_CAPTURE_LIMIT_BYTES,
    )
}

/// How long pipe workers may keep running after the supervised child is gone.
///
/// Once the child has been reaped — normal exit, timeout, cancellation, capture
/// limit, or a forwarded signal — and its process group killed, every pipe
/// normally reaches EOF at once. A descendant that left the group (`setsid`,
/// `setpgid`) can still hold a pipe open indefinitely, so the supervisor waits
/// at most this long for the workers to finish and then stops them:
///
/// - output the pipe already buffers when a reader is stopped is still read,
///   so everything written before that point is retained;
/// - output written after it is discarded, and the read end is closed;
/// - stdin bytes the holder never read are abandoned, and the write end is
///   closed.
pub(crate) const DRAIN_BUDGET: Duration = Duration::from_secs(1);

/// Supervisor half of the drain bound. Every worker holds a [`StopWatch`]
/// from [`Self::watch`]; [`Self::settle`] (or dropping this) stops them.
///
/// The stop travels over a socket pair: dropping the supervisor's end makes
/// the workers' ends readable, which each worker observes in `poll(2)`
/// alongside its own pipe. Every descriptor has exactly one owner and is
/// closed only by that owner.
pub(super) struct DrainStop {
    #[cfg(unix)]
    signal: UnixStream,
    #[cfg(unix)]
    watched: UnixStream,
    running_tx: SyncSender<()>,
    running_rx: Receiver<()>,
}

/// Worker half of the drain bound; dropping it reports the worker finished.
pub(super) struct StopWatch {
    #[cfg(unix)]
    stop: UnixStream,
    // Never sent on: the supervisor sees every worker gone once all clones
    // are dropped.
    _running: SyncSender<()>,
}

impl DrainStop {
    pub(super) fn new() -> io::Result<Self> {
        // Nothing is ever sent; the channel only reports when the last
        // worker drops its sender.
        let (running_tx, running_rx) = mpsc::sync_channel(1);
        #[cfg(unix)]
        let (signal, watched) = UnixStream::pair()?;
        Ok(Self {
            #[cfg(unix)]
            signal,
            #[cfg(unix)]
            watched,
            running_tx,
            running_rx,
        })
    }

    pub(super) fn watch(&self) -> io::Result<StopWatch> {
        Ok(StopWatch {
            #[cfg(unix)]
            stop: self.watched.try_clone()?,
            _running: self.running_tx.clone(),
        })
    }

    /// Wait up to `budget` for every worker to finish on its own, then stop
    /// the rest. Returns whether any worker had to be stopped.
    ///
    /// A stopped worker returns promptly, so joining it afterwards is
    /// bounded. Off Unix the workers use blocking I/O and cannot be stopped.
    pub(super) fn settle(self, budget: Duration) -> bool {
        let Self {
            #[cfg(unix)]
            signal,
            running_tx,
            running_rx,
            ..
        } = self;
        drop(running_tx);
        let finished = matches!(
            running_rx.recv_timeout(budget),
            Err(RecvTimeoutError::Disconnected)
        );
        #[cfg(unix)]
        drop(signal);
        !finished
    }
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

pub(super) fn spawn_stdout_drain<R>(
    out: R,
    debug: bool,
    limit: usize,
    limit_tx: SyncSender<&'static str>,
    stop: StopWatch,
) -> JoinHandle<Vec<u8>>
where
    R: Read + PipeFd + Send + 'static,
{
    spawn_drain(out, debug, limit, limit_tx, stop, "stdout")
}

pub(super) fn spawn_stderr_drain<R>(
    err: R,
    debug: bool,
    limit: usize,
    limit_tx: SyncSender<&'static str>,
    stop: StopWatch,
) -> JoinHandle<Vec<u8>>
where
    R: Read + PipeFd + Send + 'static,
{
    spawn_drain(err, debug, limit, limit_tx, stop, "stderr")
}

/// Capture a child stream's raw bytes up to `limit`; in debug mode also echo
/// it to Orbit's stderr with sensitive values redacted.
fn spawn_drain<R>(
    mut reader: R,
    debug: bool,
    limit: usize,
    limit_tx: SyncSender<&'static str>,
    stop: StopWatch,
    stream: &'static str,
) -> JoinHandle<Vec<u8>>
where
    R: Read + PipeFd + Send + 'static,
{
    thread::spawn(move || {
        let mut capture = BoundedOutputCapture::new(limit);
        let mut echo = debug.then(|| RedactingEcho::new(std::io::stderr()));
        pump(&mut reader, &stop, |bytes| {
            if let Some(echo) = echo.as_mut() {
                echo.push(bytes);
            }
            if capture.push(bytes) {
                let _ = limit_tx.send(stream);
                return true;
            }
            false
        });
        if let Some(echo) = echo {
            echo.finish();
        }
        capture.into_bytes()
    })
}

/// Forward a child stream into `relay`, whose read end a caller-supplied
/// consumer owns. Closing `relay` when the drain ends is what gives the
/// consumer EOF, so it is bound by the same [`DRAIN_BUDGET`] as captured
/// output. A consumer that drops its end stops the forwarding.
pub(super) fn spawn_relay_drain<R>(
    mut reader: R,
    mut relay: PipeWriter,
    stop: StopWatch,
) -> JoinHandle<Vec<u8>>
where
    R: Read + PipeFd + Send + 'static,
{
    thread::spawn(move || {
        pump(&mut reader, &stop, |bytes| relay.write_all(bytes).is_err());
        Vec::new()
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
        libc::pollfd {
            fd: stop.stop.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: both descriptors are borrowed from handles that outlive
        // the call, and `fds` is a valid array of the length passed.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if rc >= 0 {
            return Ok(if fds[1].revents != 0 {
                Ready::Stopped
            } else {
                Ready::Io
            });
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(unix)]
fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
    // SAFETY: `fcntl` reads and updates status flags of a descriptor this
    // worker owns; the pipe's other end has its own open file description.
    unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
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

/// A partial line held longer than this is echoed anyway, so a child that
/// never writes a newline cannot grow the buffer without bound.
const MAX_PENDING_ECHO_BYTES: usize = 64 * 1024;

/// Debug echo that redacts whole lines. Redacting each read on its own would
/// miss a secret split across two reads and garble a multi-byte character
/// split across them.
pub(super) struct RedactingEcho<W: Write> {
    sink: W,
    pending: Vec<u8>,
}

impl<W: Write> RedactingEcho<W> {
    pub(super) fn new(sink: W) -> Self {
        Self {
            sink,
            pending: Vec::new(),
        }
    }

    pub(super) fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let cut = match self.pending.iter().rposition(|&byte| byte == b'\n') {
            Some(newline) => newline + 1,
            None if self.pending.len() >= MAX_PENDING_ECHO_BYTES => self.pending.len(),
            None => return,
        };
        let complete: Vec<u8> = self.pending.drain(..cut).collect();
        self.write_redacted(&complete);
    }

    pub(super) fn finish(mut self) -> W {
        let rest = std::mem::take(&mut self.pending);
        if !rest.is_empty() {
            self.write_redacted(&rest);
        }
        self.sink
    }

    fn write_redacted(&mut self, bytes: &[u8]) {
        let redacted = redact_sensitive_env_text(&String::from_utf8_lossy(bytes));
        let _ = self.sink.write_all(redacted.as_bytes());
    }
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
