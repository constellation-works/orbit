//! The broker's bounded listener (design §4.3–§4.4).
//!
//! One accept thread authenticates each connection, then admits it to a fixed
//! pool of workers. At most [`IN_FLIGHT`] requests run and [`QUEUED`] more
//! wait; a connection beyond that is answered `plugin_broker_busy`
//! (retryable). A refused peer is closed with no reply, so nothing reveals
//! whether the socket belongs to a live run.

use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::peer::{Peer, PeerAnchor, Refusal, authenticate, reauthenticate};
use super::protocol::{
    BUSY, FrameError, INVALID_REQUEST, MAX_REQUEST_BYTES, NOT_IMPLEMENTED, REQUEST_TOO_LARGE,
    error_response, parse_request, read_frame, write_frame,
};

/// Requests a broker runs at once.
pub(crate) const IN_FLIGHT: usize = 4;
/// Requests a broker holds while all workers are busy.
pub(crate) const QUEUED: usize = 16;
/// How long a peer has to send its request, and to read the response.
const IO_TIMEOUT: Duration = Duration::from_secs(10);
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
/// requests already admitted finish on their own within [`IO_TIMEOUT`].
pub(crate) struct BrokerServer {
    stop: Arc<AtomicBool>,
    wake: UnixStream,
    accept: Option<JoinHandle<()>>,
}

impl BrokerServer {
    pub(crate) fn spawn(
        listener: UnixListener,
        anchor: Arc<AnchorSlot>,
        run_id: &str,
    ) -> io::Result<Self> {
        listener.set_nonblocking(true)?;
        let (wake, wakeup) = UnixStream::pair()?;
        let stop = Arc::new(AtomicBool::new(false));
        let admitted = Arc::new(AtomicUsize::new(0));
        let (queue, pending) = mpsc::sync_channel::<Admitted>(IN_FLIGHT + QUEUED);
        let pending = Arc::new(Mutex::new(pending));

        for _ in 0..IN_FLIGHT {
            let worker = Worker {
                pending: Arc::clone(&pending),
                admitted: Arc::clone(&admitted),
                stop: Arc::clone(&stop),
                run_id: run_id.to_string(),
            };
            thread::Builder::new()
                .name("orbit-plugin-broker-worker".to_string())
                .spawn(move || worker.run())?;
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
            loop {
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
        let body = match read_frame(&mut stream, MAX_REQUEST_BYTES) {
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
            // Forwarding to the audited plugin dispatch arrives with the
            // nested client (design §8, step 3). Until then an authenticated
            // request is answered, and nothing runs.
            Ok(_tool) => error_response(
                NOT_IMPLEMENTED,
                "the plugin broker does not run plugin calls yet; the request was not executed",
                false,
            ),
            Err(message) => error_response(INVALID_REQUEST, &message, false),
        };
        respond(stream, &response);
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
