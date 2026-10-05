use std::io::{self, PipeWriter, Read, Write};
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use orbit_common::process::output_capture::{BoundedOutputCapture, capture_limit_from_env};
use orbit_common::security::redaction::redact_sensitive_env_bytes;

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
pub(super) const MAX_PENDING_ECHO_BYTES: usize = 64 * 1024;

/// Debug echo that redacts sensitive environment values.
///
/// Bytes stay buffered through a newline so a value, or a multibyte character,
/// split across reads is still whole when it is written. A partial line is
/// flushed at [`MAX_PENDING_ECHO_BYTES`]. The longest suffix that is a proper
/// prefix of a sensitive value is kept across that flush, and across a newline
/// inside a multiline value, until the value completes. Retention stops at the
/// same cap, so a value longer than the cap can still be split.
pub(super) struct RedactingEcho<W: Write> {
    sink: W,
    pending: Vec<u8>,
    pending_newline: bool,
}

impl<W: Write> RedactingEcho<W> {
    pub(super) fn new(sink: W) -> Self {
        Self {
            sink,
            pending: Vec::new(),
            pending_newline: false,
        }
    }

    /// Bytes not yet written to the sink.
    ///
    /// Supervision tests assert this stays within [`MAX_PENDING_ECHO_BYTES`]
    /// after every read, including a partial line that follows a newline.
    #[cfg(test)]
    pub(super) fn buffered_len(&self) -> usize {
        self.pending.len()
    }

    pub(super) fn push(&mut self, bytes: &[u8]) {
        if !self.pending_newline && bytes.contains(&b'\n') {
            self.pending_newline = true;
        }
        self.pending.extend_from_slice(bytes);
        self.drain(false);
    }

    pub(super) fn finish(mut self) -> W {
        self.drain(true);
        self.sink
    }

    fn drain(&mut self, finishing: bool) {
        if finishing {
            let _holdback = redact_sensitive_env_bytes(&mut self.pending);
            if !self.pending.is_empty() {
                let rest = std::mem::take(&mut self.pending);
                let _ = self.sink.write_all(&rest);
            }
            self.pending_newline = false;
            return;
        }
        loop {
            if self.pending.is_empty() {
                self.pending_newline = false;
                return;
            }
            if !self.pending_newline && self.pending.len() < MAX_PENDING_ECHO_BYTES {
                return;
            }
            let secret_holdback = redact_sensitive_env_bytes(&mut self.pending);
            self.pending_newline = self.pending.contains(&b'\n');
            if self.pending.is_empty() {
                return;
            }
            let keep = retained_suffix(&self.pending, secret_holdback);
            if self.emit_settled(keep) {
                continue;
            }
            return;
        }
    }

    /// Write every byte that is safe to release. Returns whether a later pass
    /// should look at what remains.
    fn emit_settled(&mut self, keep: usize) -> bool {
        if self.pending.len() <= keep {
            if self.pending.len() < MAX_PENDING_ECHO_BYTES {
                return false;
            }
            let min_cut = self
                .pending
                .len()
                .saturating_sub(MAX_PENDING_ECHO_BYTES - 1)
                .max(1);
            return self.emit(utf8_progress_cut(&self.pending, min_cut));
        }
        let emit_limit = self.pending.len() - keep;
        if let Some(newline) = self.pending[..emit_limit]
            .iter()
            .rposition(|&byte| byte == b'\n')
        {
            return self.emit(newline + 1);
        }
        if self.pending.len() >= MAX_PENDING_ECHO_BYTES {
            let cut = utf8_floor_cut(&self.pending, emit_limit);
            let cut = if cut == 0 {
                utf8_progress_cut(&self.pending, 1)
            } else {
                cut
            };
            return self.emit(cut);
        }
        false
    }

    fn emit(&mut self, cut: usize) -> bool {
        if cut == 0 || cut > self.pending.len() {
            return false;
        }
        let chunk: Vec<u8> = self.pending.drain(..cut).collect();
        let _ = self.sink.write_all(&chunk);
        self.pending_newline = self.pending.contains(&b'\n');
        true
    }
}

/// Bytes held back so a sensitive prefix, and a trailing incomplete UTF-8
/// sequence, are not written before the rest of the value or character arrives.
fn retained_suffix(bytes: &[u8], secret_holdback: usize) -> usize {
    secret_holdback
        .max(incomplete_utf8_tail(bytes))
        .min(MAX_PENDING_ECHO_BYTES.saturating_sub(1))
        .min(bytes.len())
}

fn incomplete_utf8_tail(bytes: &[u8]) -> usize {
    let len = bytes.len();
    if len == 0 {
        return 0;
    }
    let start = len.saturating_sub(3);
    for index in (start..len).rev() {
        if !is_utf8_boundary(bytes, index) {
            continue;
        }
        let width = utf8_sequence_len(bytes[index]);
        let have = len - index;
        if width > have && have < 4 {
            return have;
        }
        return 0;
    }
    0
}

fn utf8_sequence_len(lead: u8) -> usize {
    if lead & 0b1000_0000 == 0 {
        1
    } else if lead & 0b1110_0000 == 0b1100_0000 {
        2
    } else if lead & 0b1111_0000 == 0b1110_0000 {
        3
    } else if lead & 0b1111_1000 == 0b1111_0000 {
        4
    } else {
        0
    }
}

fn is_utf8_boundary(bytes: &[u8], index: usize) -> bool {
    index == 0 || index >= bytes.len() || bytes[index] & 0b1100_0000 != 0b1000_0000
}

/// Largest cut at or before `cut` that does not split a UTF-8 sequence.
fn utf8_floor_cut(bytes: &[u8], mut cut: usize) -> usize {
    cut = cut.min(bytes.len());
    while cut > 0 && !is_utf8_boundary(bytes, cut) {
        cut -= 1;
    }
    cut
}

/// Smallest cut at or after `min_cut` that does not split a UTF-8 sequence.
fn utf8_progress_cut(bytes: &[u8], min_cut: usize) -> usize {
    if bytes.is_empty() || min_cut == 0 {
        return 0;
    }
    let mut cut = min_cut.min(bytes.len());
    while cut < bytes.len() && !is_utf8_boundary(bytes, cut) {
        cut += 1;
    }
    cut
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
