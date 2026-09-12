//! Bounded reads from the process stdin for `orbit init` prompts.
//!
//! A human at a TTY may take as long as they need. A non-TTY stdin (pipe,
//! socket, redirected file) is given a short window to deliver a line so an
//! agent harness that leaves stdin open cannot hang forever holding the init
//! lock. The bound is on waiting, not on descriptor kind alone: a pipe that
//! already has answers still completes.
//!
//! The in-memory `BufRead` helpers in `command.rs` stay unbounded; this
//! module is the real-stdin boundary only.

use std::io::{self, ErrorKind, Write};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use crate::output::sink;

pub(super) const STDIN_CLOSED_BEFORE_PROMPT: &str = "stdin closed before an interactive prompt was answered; pass --task-prefix/--host-name or --non-interactive";

pub(super) const STDIN_PROMPT_TIMEOUT: &str = "stdin did not answer an interactive prompt; pass --task-prefix/--host-name or --non-interactive";

/// Ceiling for one non-TTY prompt. Piped answers are already in the kernel
/// buffer, so a working pipe returns immediately; this only trips when the
/// descriptor stays silent.
pub(super) const NON_TTY_PROMPT_TIMEOUT: Duration = Duration::from_secs(2);

struct StdinLeftover {
    buf: Vec<u8>,
}

fn leftover() -> MutexGuard<'static, StdinLeftover> {
    static STATE: Mutex<StdinLeftover> = Mutex::new(StdinLeftover { buf: Vec::new() });
    STATE.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Write `prompt` to `output` and read one trimmed line from process stdin.
pub(super) fn read_trimmed_line(prompt: &str, output: &mut impl Write) -> io::Result<String> {
    write!(output, "{prompt}")?;
    output.flush()?;

    let timeout = if sink::stdin_is_terminal() {
        None
    } else {
        Some(NON_TTY_PROMPT_TIMEOUT)
    };
    read_stdin_line(timeout)
}

fn read_stdin_line(timeout: Option<Duration>) -> io::Result<String> {
    let mut leftover = leftover();
    loop {
        if let Some(line) = take_complete_line(&mut leftover.buf) {
            return Ok(line);
        }
        if let Some(timeout) = timeout {
            wait_for_stdin(timeout)?;
        }
        match fill_leftover(&mut leftover.buf)? {
            Fill::More => {}
            Fill::Eof => {
                return Ok(take_trailing_line(&mut leftover.buf));
            }
        }
    }
}

enum Fill {
    More,
    Eof,
}

fn fill_leftover(buf: &mut Vec<u8>) -> io::Result<Fill> {
    let mut chunk = [0u8; 1024];
    let n = read_stdin_bytes(&mut chunk)?;
    if n == 0 {
        if buf.is_empty() {
            return Err(closed_stdin());
        }
        return Ok(Fill::Eof);
    }
    buf.extend_from_slice(&chunk[..n]);
    Ok(Fill::More)
}

fn take_complete_line(buf: &mut Vec<u8>) -> Option<String> {
    let idx = buf.iter().position(|&b| b == b'\n')?;
    let mut line: Vec<u8> = buf.drain(..=idx).collect();
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Some(utf8_line(line))
}

fn take_trailing_line(buf: &mut Vec<u8>) -> String {
    utf8_line(std::mem::take(buf))
}

fn utf8_line(line: Vec<u8>) -> String {
    String::from_utf8_lossy(&line).trim().to_string()
}

fn closed_stdin() -> io::Error {
    io::Error::new(ErrorKind::UnexpectedEof, STDIN_CLOSED_BEFORE_PROMPT)
}

#[cfg(unix)]
fn timed_out_stdin() -> io::Error {
    io::Error::new(ErrorKind::TimedOut, STDIN_PROMPT_TIMEOUT)
}

fn wait_for_stdin(timeout: Duration) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        wait_for_fd(io::stdin().as_raw_fd(), timeout)
    }
    #[cfg(not(unix))]
    {
        let _ = timeout;
        Ok(())
    }
}

fn read_stdin_bytes(buf: &mut [u8]) -> io::Result<usize> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        read_fd(io::stdin().as_raw_fd(), buf)
    }
    #[cfg(not(unix))]
    {
        use std::io::Read;
        io::stdin().read(buf)
    }
}

#[cfg(unix)]
pub(super) fn wait_for_fd(fd: std::os::fd::RawFd, timeout: Duration) -> io::Result<()> {
    let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    let mut fds = [libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }];
    loop {
        // SAFETY: `fds` is a one-element pollfd array we own for the call;
        // `fd` is a live descriptor (process stdin or a test socket).
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout_ms) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if rc == 0 {
            return Err(timed_out_stdin());
        }
        return Ok(());
    }
}

#[cfg(unix)]
fn read_fd(fd: std::os::fd::RawFd, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        // SAFETY: `fd` is a live descriptor; `buf` is a valid writable slice
        // we own for `buf.len()` bytes.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        return Ok(n as usize);
    }
}
