//! Read readiness from the owned server's bind announcement, never a TCP probe.

use std::collections::VecDeque;
use std::io::{self, Read};
use std::net::SocketAddr;
use std::ops::{Deref, DerefMut};
use std::process::{Child, Command};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::child_guard::ChildGuard;

const LINE_LIMIT: usize = 16 * 1024;

#[derive(Clone, Copy)]
pub(crate) enum Announcement {
    #[allow(dead_code)]
    Dashboard,
    // The process and tool integration binaries only start dashboard fixtures.
    #[allow(dead_code)]
    Mcp,
}

impl Announcement {
    fn address(self, line: &str) -> Option<SocketAddr> {
        match self {
            Self::Dashboard => line
                .trim()
                .strip_prefix("Dashboard listening on http://")?
                .parse()
                .ok(),
            Self::Mcp => line
                .split_once("orbit mcp listener bound")?
                .1
                .split_whitespace()
                .find_map(|field| field.strip_prefix("address=")?.parse().ok()),
        }
        .filter(|addr: &SocketAddr| addr.ip().is_loopback() && addr.port() != 0)
    }
}

pub(crate) struct BoundServer {
    // Reap the server and terminate its owned group before joining readers.
    child: ChildGuard,
    output: Output,
    addresses: mpsc::Receiver<SocketAddr>,
}

impl BoundServer {
    pub(crate) fn spawn(command: &mut Command, announcement: Announcement) -> io::Result<Self> {
        let mut child = ChildGuard::spawn(command)?;
        let (sender, addresses) = mpsc::sync_channel(8);
        let mut output = Output::default();
        match announcement {
            Announcement::Dashboard => output.read(
                child.stdout.take().expect("piped dashboard stdout"),
                Some((announcement, sender)),
            ),
            Announcement::Mcp => output.read(
                child.stderr.take().expect("piped MCP stderr"),
                Some((announcement, sender)),
            ),
        }
        if let Some(stdout) = child.stdout.take() {
            output.read(stdout, None);
        }
        if let Some(stderr) = child.stderr.take() {
            output.read(stderr, None);
        }
        Ok(Self {
            child,
            output,
            addresses,
        })
    }

    pub(crate) fn wait_for_address(&mut self, timeout: Duration) -> io::Result<SocketAddr> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.output.join(deadline);
                return Err(self.failure(format!("server exited before readiness: {status}")));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.output.join(deadline);
                return Err(self.failure(format!("no bind announcement within {timeout:?}")));
            }
            match self
                .addresses
                .recv_timeout(remaining.min(Duration::from_millis(20)))
            {
                Ok(address) => {
                    // An announcement queued before an exit cannot establish
                    // readiness for a child that has already been reaped.
                    if let Some(status) = self.child.try_wait()? {
                        self.output.join(deadline);
                        return Err(
                            self.failure(format!("server exited after announcing: {status}"))
                        );
                    }
                    return Ok(address);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // EOF can precede waitpid visibility. Keep the same deadline
                    // while the child exits and the diagnostic reader drains.
                    std::thread::sleep(remaining.min(Duration::from_millis(20)));
                }
            }
        }
    }

    fn failure(&self, reason: String) -> io::Error {
        io::Error::other(format!(
            "{reason}; pid={}; {}; output={:?}",
            self.child.id(),
            orbit_common::test_env::host_load(),
            self.output.tail.lock().expect("output lock")
        ))
    }
}

impl Deref for BoundServer {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.child
    }
}

impl DerefMut for BoundServer {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

impl Drop for BoundServer {
    fn drop(&mut self) {
        // Reader cancellation and child cleanup share a single teardown budget.
        self.output
            .finish_by(Instant::now() + Duration::from_secs(2));
    }
}

#[derive(Default)]
struct Output {
    readers: Vec<JoinHandle<()>>,
    tail: Arc<Mutex<VecDeque<String>>>,
    finish: Arc<(Mutex<Option<Instant>>, Condvar)>,
}

// Readers keep exclusive ownership of their pipe throughout cancellation.
#[cfg(unix)]
trait Pipe: Read + std::os::fd::AsRawFd + Send + 'static {}
#[cfg(unix)]
impl<T: Read + std::os::fd::AsRawFd + Send + 'static> Pipe for T {}
#[cfg(windows)]
trait Pipe: Read + std::os::windows::io::AsRawHandle + Send + 'static {}
#[cfg(windows)]
impl<T: Read + std::os::windows::io::AsRawHandle + Send + 'static> Pipe for T {}

impl Output {
    fn read(
        &mut self,
        mut pipe: impl Pipe,
        mut announcement: Option<(Announcement, mpsc::SyncSender<SocketAddr>)>,
    ) {
        let tail = Arc::clone(&self.tail);
        let finish = Arc::clone(&self.finish);
        self.readers.push(std::thread::spawn(move || {
            #[cfg(unix)]
            {
                // SAFETY: this thread exclusively owns the live pipe descriptor.
                let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
                // SAFETY: the same exclusively owned live descriptor.
                if flags < 0
                    || unsafe {
                        libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK)
                    } != 0
                {
                    record(
                        &tail,
                        format!(
                            "nonblocking output setup failed: {}",
                            io::Error::last_os_error()
                        ),
                    );
                    return;
                }
            }
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            let mut discard = false;
            loop {
                if finish
                    .0
                    .lock()
                    .expect("finish lock")
                    .is_some_and(|end| Instant::now() >= end)
                {
                    break;
                }
                match read_available(&mut pipe, &mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        if discard {
                            continue;
                        }
                        for byte in &buffer[..count] {
                            bytes.push(*byte);
                            if bytes.len() > LINE_LIMIT {
                                record(
                                    &tail,
                                    format!(
                                        "invalid announcement output (line limit {LINE_LIMIT})"
                                    ),
                                );
                                bytes.clear();
                                announcement = None;
                                discard = true;
                                break;
                            }
                            if *byte == b'\n' {
                                emit(&tail, &announcement, &bytes);
                                bytes.clear();
                            }
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        let end = finish.0.lock().expect("finish lock");
                        let wait = end.map_or(Duration::from_millis(10), |end| {
                            end.saturating_duration_since(Instant::now())
                                .min(Duration::from_millis(10))
                        });
                        let _ = finish
                            .1
                            .wait_timeout(end, wait)
                            .expect("reader cancellation wait");
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        record(&tail, format!("output read failed: {error}"));
                        break;
                    }
                }
            }
            if !bytes.is_empty() {
                emit(&tail, &announcement, &bytes);
            }
            // Dropping here closes only this reader's descriptor, then join
            // confirms that no pipe-reading thread survives finalization.
        }));
    }

    fn finish_by(&self, deadline: Instant) {
        let mut end = self.finish.0.lock().expect("finish lock");
        *end = Some(end.map_or(deadline, |previous| previous.min(deadline)));
        self.finish.1.notify_all();
    }

    fn join(&mut self, deadline: Instant) {
        self.finish_by(deadline);
        for reader in self.readers.drain(..) {
            if reader.join().is_err() {
                record(&self.tail, "output reader panicked".into());
            }
        }
    }
}

fn emit(
    tail: &Mutex<VecDeque<String>>,
    announcement: &Option<(Announcement, mpsc::SyncSender<SocketAddr>)>,
    bytes: &[u8],
) {
    let line = String::from_utf8_lossy(bytes);
    if let Some((kind, sender)) = announcement
        && let Some(address) = kind.address(&line)
    {
        // Continue draining after readiness and across exec; output must
        // never block the server on a full pipe.
        let _ = sender.try_send(address);
    }
    record(tail, line.into_owned());
}

fn record(tail: &Mutex<VecDeque<String>>, line: String) {
    let mut tail = tail.lock().expect("output lock");
    if tail.len() == 8 {
        tail.pop_front();
    }
    tail.push_back(line);
}

#[cfg(unix)]
fn read_available(pipe: &mut impl Pipe, buffer: &mut [u8]) -> io::Result<usize> {
    pipe.read(buffer)
}

#[cfg(windows)]
fn read_available(pipe: &mut impl Pipe, buffer: &mut [u8]) -> io::Result<usize> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn PeekNamedPipe(
            pipe: *mut std::ffi::c_void,
            buffer: *mut std::ffi::c_void,
            size: u32,
            read: *mut u32,
            available: *mut u32,
            left: *mut u32,
        ) -> i32;
    }
    let mut available = 0;
    // SAFETY: the reader owns this handle. Peek never waits, writes only
    // available, and no competing reader can consume the reported bytes.
    let ok = unsafe {
        PeekNamedPipe(
            pipe.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(109) {
            Ok(0)
        } else {
            Err(error)
        };
    }
    if available == 0 {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    let count = buffer.len().min(available as usize);
    pipe.read(&mut buffer[..count])
}

impl Drop for Output {
    fn drop(&mut self) {
        self.join(Instant::now() + Duration::from_secs(2));
    }
}
