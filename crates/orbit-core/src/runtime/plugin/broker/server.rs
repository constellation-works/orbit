//! The broker's bounded listener (design §4.3–§4.4).
//!
//! One accept thread authenticates each connection, then admits it to a fixed
//! pool of workers. At most [`IN_FLIGHT`] requests run and [`QUEUED`] more
//! wait; a connection beyond that is answered `plugin_broker_busy`
//! (retryable). A refused peer is closed with no reply, so nothing reveals
//! whether the socket belongs to a live run.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::BrokerDispatch;
use super::peer::{Peer, PeerAnchor, Refusal, authenticate, reauthenticate};
use super::protocol::{
    BUSY, FrameError, INVALID_REQUEST, MAX_REQUEST_BYTES, REQUEST_TOO_LARGE, call_error_response,
    error_response, output_response, parse_request, read_frame, write_frame,
};

/// Requests a broker runs at once.
const IN_FLIGHT: usize = 4;
/// Requests a broker holds while all workers are busy.
const QUEUED: usize = 16;
/// Total time to receive a frame once a worker starts reading it; also the
/// per-write timeout for responses.
pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a connection that arrives before the sandbox is identified waits
/// for it. The agent cannot connect before it is spawned, so this only
/// covers the moments between spawn and [`AnchorSlot::bind`].
const ANCHOR_WAIT: Duration = Duration::from_secs(10);

/// The sandbox identity peers are checked against, set once after spawn.
#[derive(Debug, Default)]
pub(crate) struct AnchorSlot {
    state: Mutex<AnchorState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
enum AnchorState {
    #[default]
    Pending,
    Bound(Arc<PeerAnchor>),
    /// The sandbox could not be identified, or the broker is shutting down.
    Closed,
}

impl AnchorSlot {
    pub(crate) fn bind(&self, anchor: PeerAnchor) {
        self.set(AnchorState::Bound(Arc::new(anchor)));
    }

    pub(crate) fn close(&self) {
        self.set(AnchorState::Closed);
    }

    fn set(&self, next: AnchorState) {
        if let Ok(mut state) = self.state.lock() {
            if matches!(*state, AnchorState::Closed) {
                return;
            }
            *state = next;
        }
        self.changed.notify_all();
    }

    fn wait(&self, timeout: Duration) -> Option<Arc<PeerAnchor>> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().ok()?;
        loop {
            match &*state {
                AnchorState::Bound(anchor) => return Some(Arc::clone(anchor)),
                AnchorState::Closed => return None,
                AnchorState::Pending => {}
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            state = self.changed.wait_timeout(state, remaining).ok()?.0;
        }
    }
}

/// A running listener. Dropping it stops accepting and closes the listener;
/// admitted calls are cancelled and workers joined before teardown returns.
pub(crate) struct BrokerServer {
    stop: Arc<AtomicBool>,
    wake: UnixStream,
    accept: Option<JoinHandle<()>>,
    workers: Vec<JoinHandle<()>>,
}

impl BrokerServer {
    pub(crate) fn spawn(
        listener: UnixListener,
        anchor: Arc<AnchorSlot>,
        dispatch: Arc<dyn BrokerDispatch>,
        run_id: &str,
    ) -> io::Result<Self> {
        listener.set_nonblocking(true)?;
        let (wake, wakeup) = UnixStream::pair()?;
        let stop = Arc::new(AtomicBool::new(false));
        let admitted = Arc::new(AtomicUsize::new(0));
        let (queue, pending) = mpsc::sync_channel::<Admitted>(IN_FLIGHT + QUEUED);
        let pending = Arc::new(Mutex::new(pending));

        let mut workers = Vec::new();
        for _ in 0..IN_FLIGHT {
            let worker = Worker {
                pending: Arc::clone(&pending),
                admitted: Arc::clone(&admitted),
                stop: Arc::clone(&stop),
                dispatch: Arc::clone(&dispatch),
                run_id: run_id.to_string(),
            };
            workers.push(
                thread::Builder::new()
                    .name("orbit-plugin-broker-worker".to_string())
                    .spawn(move || worker.run())?,
            );
        }

        let acceptor = Acceptor {
            listener,
            wakeup,
            anchor,
            queue,
            admitted,
            stop: Arc::clone(&stop),
            run_id: run_id.to_string(),
        };
        let accept = thread::Builder::new()
            .name("orbit-plugin-broker".to_string())
            .spawn(move || acceptor.run())?;
        Ok(Self {
            stop,
            wake,
            accept: Some(accept),
            workers,
        })
    }
}

impl Drop for BrokerServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = (&self.wake).write_all(&[1]);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// An authenticated connection waiting for a worker.
struct Admitted {
    stream: UnixStream,
    peer: Peer,
    anchor: Arc<PeerAnchor>,
}

struct Acceptor {
    listener: UnixListener,
    wakeup: UnixStream,
    anchor: Arc<AnchorSlot>,
    queue: SyncSender<Admitted>,
    admitted: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    run_id: String,
}

impl Acceptor {
    fn run(self) {
        while !self.stop.load(Ordering::SeqCst) {
            if !self.wait_readable() {
                break;
            }
            while !self.stop.load(Ordering::SeqCst) {
                match self.listener.accept() {
                    Ok((stream, _)) => self.admit(stream),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => {
                        tracing::warn!(
                            target: "orbit.plugin_broker",
                            run_id = %self.run_id,
                            error = %error,
                            "plugin broker accept failed"
                        );
                        break;
                    }
                }
            }
        }
    }

    /// Block until a connection is pending or shutdown was requested.
    fn wait_readable(&self) -> bool {
        let mut fds = [
            libc::pollfd {
                fd: self.listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.wakeup.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            // SAFETY: `fds` is a valid array of two initialized pollfds.
            let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
            if rc >= 0 {
                return fds[1].revents == 0 && !self.stop.load(Ordering::SeqCst);
            }
            if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return false;
            }
        }
    }

    fn admit(&self, stream: UnixStream) {
        // An accepted socket may inherit the listener's non-blocking flag.
        if stream.set_nonblocking(false).is_err()
            || stream.set_read_timeout(Some(IO_TIMEOUT)).is_err()
            || stream.set_write_timeout(Some(IO_TIMEOUT)).is_err()
        {
            return;
        }
        let Some(anchor) = self.anchor.wait(ANCHOR_WAIT) else {
            log_refusal(
                &self.run_id,
                &Refusal {
                    pid: None,
                    reason: "this run's sandbox is not identified".to_string(),
                },
            );
            return;
        };
        let peer = match authenticate(&stream, &anchor) {
            Ok(peer) => peer,
            Err(refusal) => {
                log_refusal(&self.run_id, &refusal);
                return;
            }
        };
        if self.admitted.fetch_add(1, Ordering::SeqCst) >= IN_FLIGHT + QUEUED {
            self.admitted.fetch_sub(1, Ordering::SeqCst);
            respond(
                stream,
                &error_response(
                    BUSY,
                    &format!(
                        "the plugin broker is running {IN_FLIGHT} calls and queueing {QUEUED}; \
                         retry shortly"
                    ),
                    true,
                ),
            );
            return;
        }
        let admitted = Admitted {
            stream,
            peer,
            anchor,
        };
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) =
            self.queue.try_send(admitted)
        {
            self.admitted.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

struct Worker {
    pending: Arc<Mutex<Receiver<Admitted>>>,
    admitted: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    dispatch: Arc<dyn BrokerDispatch>,
    run_id: String,
}

impl Worker {
    fn run(self) {
        loop {
            let next = match self.pending.lock() {
                Ok(pending) => pending.recv(),
                Err(_) => return,
            };
            let Ok(admitted) = next else {
                return;
            };
            if !self.stop.load(Ordering::SeqCst) {
                self.serve(admitted);
            }
            self.admitted.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn serve(&self, admitted: Admitted) {
        let Admitted {
            mut stream,
            peer,
            anchor,
        } = admitted;
        let mut reader = RequestReader {
            stream: &mut stream,
            stop: &self.stop,
            deadline: Instant::now() + IO_TIMEOUT,
        };
        let body = match read_frame(&mut reader, MAX_REQUEST_BYTES) {
            Ok(body) => body,
            Err(FrameError::TooLarge { declared }) => {
                respond(
                    stream,
                    &error_response(
                        REQUEST_TOO_LARGE,
                        &format!(
                            "request declares {declared} bytes; the broker reads at most \
                             {MAX_REQUEST_BYTES}"
                        ),
                        false,
                    ),
                );
                return;
            }
            Err(FrameError::Io(error)) => {
                tracing::debug!(
                    target: "orbit.plugin_broker",
                    run_id = %self.run_id,
                    peer_pid = peer.pid,
                    error = %error,
                    "plugin broker peer sent no complete request"
                );
                return;
            }
        };
        if let Err(refusal) = reauthenticate(&peer, &anchor) {
            log_refusal(&self.run_id, &refusal);
            return;
        }
        let response = match parse_request(&body) {
            Ok(request) => match self.call(&stream, request, peer.pid) {
                Ok(output) => output_response(output),
                Err(error) => call_error_response(&error),
            },
            Err(message) => error_response(INVALID_REQUEST, &message, false),
        };
        respond(stream, &response);
    }

    fn call(
        &self,
        stream: &UnixStream,
        request: super::BrokerRequest,
        peer_pid: u32,
    ) -> Result<serde_json::Value, orbit_common::OrbitError> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let finished = AtomicBool::new(false);
        thread::scope(|scope| {
            // One request per connection. EOF, a socket error, or extra input
            // ends that call. No signal is sent here: the backend owner kills
            // its process group before reaping, so a stale PID is never retained.
            scope.spawn(|| {
                let mut fd = libc::pollfd {
                    fd: stream.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                while !finished.load(Ordering::SeqCst) {
                    // SAFETY: fd is initialized and remains open for this scope.
                    let ready = unsafe { libc::poll(&mut fd, 1, 50) };
                    if self.stop.load(Ordering::SeqCst) || ready > 0 {
                        cancelled.store(true, Ordering::SeqCst);
                        break;
                    }
                    if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
                    {
                        cancelled.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            });
            struct Finish<'a>(&'a AtomicBool);
            impl Drop for Finish<'_> {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            let _finish = Finish(&finished);
            self.dispatch
                .call(request, peer_pid, Arc::clone(&cancelled))
        })
    }
}

/// Keep the framing parser generic while bounding every socket read, including
/// reads that continuously make progress. One deadline covers prefix and body.
struct RequestReader<'a> {
    stream: &'a mut UnixStream,
    stop: &'a AtomicBool,
    deadline: Instant,
}

impl RequestReader<'_> {
    fn remaining(&self) -> io::Result<Duration> {
        if self.stop.load(Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "broker stopped",
            ));
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "request deadline elapsed",
            ));
        }
        Ok(remaining)
    }
}

impl Read for RequestReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let remaining = self.remaining()?;
            // Short waits bound shutdown latency even when no bytes arrive.
            self.stream
                .set_read_timeout(Some(remaining.min(Duration::from_millis(50))))?;
            match self.stream.read(buf) {
                Ok(count) => {
                    self.remaining()?;
                    return Ok(count);
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }
}

fn respond(mut stream: UnixStream, body: &[u8]) {
    let _ = write_frame(&mut stream, body);
    let _ = stream.shutdown(std::net::Shutdown::Write);
}

fn log_refusal(run_id: &str, refusal: &Refusal) {
    tracing::warn!(
        target: "orbit.plugin_broker",
        run_id,
        peer_pid = refusal.pid,
        reason = %refusal.reason,
        "plugin broker refused a connection"
    );
}
