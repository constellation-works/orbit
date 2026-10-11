//! Read readiness from the owned server's bind announcement, never a TCP probe.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Read};
use std::net::SocketAddr;
use std::ops::{Deref, DerefMut};
use std::process::Child;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::child_guard::ChildGuard;

const LINE_LIMIT: usize = 16 * 1024;

#[derive(Clone, Copy)]
pub(crate) enum Announcement {
    Dashboard,
    // The tool integration binary only starts dashboard fixtures.
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

pub(crate) struct BoundServer<G = ChildGuard> {
    // Fields drop in declaration order: reap the child before joining its
    // pipe readers, including during a failed readiness assertion.
    child: G,
    output: Output,
    addresses: mpsc::Receiver<SocketAddr>,
}

impl BoundServer {
    pub(crate) fn new(child: Child, announcement: Announcement) -> Self {
        Self::with_guard(ChildGuard::new(child), announcement)
    }
}

impl<G: Deref<Target = Child> + DerefMut> BoundServer<G> {
    pub(crate) fn with_guard(mut child: G, announcement: Announcement) -> Self {
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
        if let Some(stderr) = child.stderr.take() {
            output.read(stderr, None);
        }
        Self {
            child,
            output,
            addresses,
        }
    }

    pub(crate) fn wait_for_address(&mut self, timeout: Duration) -> io::Result<SocketAddr> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.output.join();
                return Err(self.failure(format!("server exited before readiness: {status}")));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
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
                        self.output.join();
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

impl<G: Deref<Target = Child>> Deref for BoundServer<G> {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.child
    }
}

impl<G: Deref<Target = Child> + DerefMut> DerefMut for BoundServer<G> {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.child
    }
}

#[derive(Default)]
struct Output {
    readers: Vec<JoinHandle<()>>,
    tail: Arc<Mutex<VecDeque<String>>>,
}

impl Output {
    fn read(
        &mut self,
        pipe: impl Read + Send + 'static,
        announcement: Option<(Announcement, mpsc::SyncSender<SocketAddr>)>,
    ) {
        let tail = Arc::clone(&self.tail);
        self.readers.push(std::thread::spawn(move || {
            let mut reader = BufReader::new(pipe);
            loop {
                let mut bytes = Vec::new();
                match reader
                    .by_ref()
                    .take((LINE_LIMIT + 1) as u64)
                    .read_until(b'\n', &mut bytes)
                {
                    Ok(0) => break,
                    Ok(_) if bytes.len() <= LINE_LIMIT => {
                        let line = String::from_utf8_lossy(&bytes);
                        if let Some((kind, sender)) = &announcement
                            && let Some(address) = kind.address(&line)
                        {
                            // Continue draining after readiness and across exec;
                            // output must never block the server on a full pipe.
                            let _ = sender.try_send(address);
                        }
                        let mut tail = tail.lock().expect("output lock");
                        if tail.len() == 8 {
                            tail.pop_front();
                        }
                        tail.push_back(line.into_owned());
                    }
                    result => {
                        tail.lock().expect("output lock").push_back(format!(
                            "invalid announcement output (line limit {LINE_LIMIT}): {result:?}"
                        ));
                        drop(announcement);
                        let _ = io::copy(&mut reader, &mut io::sink());
                        break;
                    }
                }
            }
        }));
    }

    fn join(&mut self) {
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.join();
    }
}
