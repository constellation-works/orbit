//! Idle routed SSH sessions kept for the next call to the same destination.
//!
//! What is reused is the transport only: the `ssh` child and the remote
//! `orbit mcp serve` process it started, with its handshake. Every call still
//! runs live discovery, identity confirmation and `tools/list` on the session,
//! and no snapshot, tool surface or health verdict passes from one call to the
//! next. Listing never uses the pool.
//!
//! A parked session is still a live remote runtime, and a destination may
//! admit only one of those at a time (its store/runtime lock), so the pool
//! keeps **one** parked session per destination across every key and closes
//! parked sessions for a destination before a fresh one is opened to it. An
//! earlier version parked per key and let an idle parked runtime hold the
//! destination against the next fresh session, whose `tools/call` then waited
//! out its whole delivery budget; see the federated-mcp decisions doc.

use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use super::session::DestinationSession;
use crate::remote::McpSessionAuthority;

/// How long a parked session waits for its next call before it is closed.
///
/// The generated SSH config keeps an idle connection master for the same
/// 60 seconds (`ControlPersist 60`), so an idle destination holds nothing
/// open past the window the transport already used.
pub(super) const ROUTED_SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Everything that shapes a routed session: the argv `ssh` runs and the
/// binding its handshake sent. Two calls share a session only when all of it
/// matches, so a reused session is indistinguishable from a fresh one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RoutedSessionKey {
    pub(super) machine_id: String,
    pub(super) ssh_target: String,
    pub(super) caller_machine_id: String,
    pub(super) orchestrator: Option<String>,
    pub(super) authority: McpSessionAuthority,
    pub(super) internal: bool,
    pub(super) worker_host: bool,
    pub(super) worker_invocation: Option<orbit_types::tool::WorkerInvocation>,
    pub(super) line_cap: Option<u64>,
}

impl RoutedSessionKey {
    /// Whether two keys reach the same destination runtime, whatever shape
    /// of session each asked for.
    fn same_destination(&self, other: &Self) -> bool {
        self.machine_id == other.machine_id && self.ssh_target == other.ssh_target
    }
}

/// Routed sessions parked between calls, shared by every
/// [`super::SshDestinationProbe`] built over it.
///
/// A session is checked out for exactly one call — checkout removes it, so
/// two in-flight calls never share one — and parked again only when that
/// call's answer arrived intact with nothing left unread. The pool parks at
/// most one session per destination; parking closes any other session parked
/// for that destination, and closing idle sessions does the
/// same before a fresh session opens. Parked sessions are closed after 60
/// idle seconds (the SSH `ControlPersist` window) and when the pool is
/// dropped. A long-lived host that builds a probe per call shares one pool
/// across those probes so reuse outlives each of them.
#[derive(Clone, Default)]
pub struct RoutedSessionPool {
    shared: Arc<PoolShared>,
}

#[derive(Default)]
struct PoolShared {
    state: Mutex<PoolState>,
}

#[derive(Default)]
struct PoolState {
    idle: Vec<IdleSession>,
    reaper_running: bool,
}

struct IdleSession {
    key: RoutedSessionKey,
    session: DestinationSession,
    parked_at: Instant,
}

impl std::fmt::Debug for RoutedSessionPool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RoutedSessionPool")
            .finish_non_exhaustive()
    }
}

impl RoutedSessionPool {
    /// The parked session for `key`, if any. Expired or exited sessions are
    /// closed on the way; the caller still health-checks what it gets,
    /// because a process can be alive yet desynchronised.
    pub(super) fn checkout(&self, key: &RoutedSessionKey) -> Option<DestinationSession> {
        let mut closed = Vec::new();
        let found = {
            let mut state = self.lock();
            let now = Instant::now();
            closed.extend(take_where(&mut state.idle, |idle| {
                expired(idle, now) || idle.session.has_exited()
            }));
            state
                .idle
                .iter()
                .rposition(|idle| idle.key == *key)
                .map(|index| state.idle.remove(index).session)
        };
        // Closing kills and reaps the child; never under the lock.
        drop(closed);
        found
    }

    /// Close every session parked for `key`'s destination. Called before a
    /// fresh session opens to that destination, so an idle parked runtime
    /// cannot hold the destination's runtime lock against the fresh one.
    pub(super) fn close_idle_for_destination(&self, key: &RoutedSessionKey) {
        let closed = {
            let mut state = self.lock();
            take_where(&mut state.idle, |idle| idle.key.same_destination(key))
        };
        drop(closed);
    }

    /// Park a session whose last call was answered intact, closing any other
    /// session already parked for the same destination: one destination
    /// holds at most one parked runtime.
    pub(super) fn park(&self, key: RoutedSessionKey, session: DestinationSession) {
        let (closed, start_reaper) = {
            let mut state = self.lock();
            let closed = take_where(&mut state.idle, |idle| idle.key.same_destination(&key));
            state.idle.push(IdleSession {
                key,
                session,
                parked_at: Instant::now(),
            });
            let start_reaper = !std::mem::replace(&mut state.reaper_running, true);
            (closed, start_reaper)
        };
        drop(closed);
        if start_reaper {
            let shared = Arc::downgrade(&self.shared);
            std::thread::spawn(move || reap_idle_sessions(&shared));
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PoolState> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

fn expired(idle: &IdleSession, now: Instant) -> bool {
    now.saturating_duration_since(idle.parked_at) >= ROUTED_SESSION_IDLE_TIMEOUT
}

fn take_where(
    idle: &mut Vec<IdleSession>,
    mut condemned: impl FnMut(&mut IdleSession) -> bool,
) -> Vec<IdleSession> {
    let mut taken = Vec::new();
    let mut index = 0;
    while index < idle.len() {
        if condemned(&mut idle[index]) {
            taken.push(idle.remove(index));
        } else {
            index += 1;
        }
    }
    taken
}

/// Close parked sessions as they pass the idle window; ends once none are
/// parked or the pool is gone. Holds the pool only while sweeping, so dropping
/// the pool closes its sessions at once.
fn reap_idle_sessions(shared: &Weak<PoolShared>) {
    let mut wait = ROUTED_SESSION_IDLE_TIMEOUT;
    loop {
        std::thread::sleep(wait);
        let Some(shared) = shared.upgrade() else {
            return;
        };
        let now = Instant::now();
        let (closed, next) = {
            let mut state = shared.state.lock().unwrap_or_else(PoisonError::into_inner);
            let closed = take_where(&mut state.idle, |idle| expired(idle, now));
            let next = state
                .idle
                .iter()
                .map(|idle| {
                    (idle.parked_at + ROUTED_SESSION_IDLE_TIMEOUT).saturating_duration_since(now)
                })
                .min();
            if next.is_none() {
                state.reaper_running = false;
            }
            (closed, next)
        };
        drop(closed);
        drop(shared);
        match next {
            // A small floor so sessions parked together close in one sweep.
            Some(next) => wait = next.max(Duration::from_millis(50)),
            None => return,
        }
    }
}
