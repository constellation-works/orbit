//! Per-host SSH tunnels behind `/api/on/<host>/…` [ORB-14679].
//!
//! The dashboard forwards a request for a registered host to that host's own
//! `orbit web serve` through Web's SSH local forward ([`crate::ssh_tunnel`]).
//! This module owns those forwards and nothing else; the HTTP side lives in
//! `api::forward`.
//!
//! Lifecycle (remote-access spec `host-forward.md`):
//!
//! - One tunnel per host, keyed by the entry's `machine_id`, opened by the
//!   first request that needs it. Establishing is single-flight: concurrent
//!   first requests wait for one establish and share its result.
//! - A tunnel is reused while its `ssh` child lives. A dead child is replaced
//!   by the next request. Nothing reconnects or probes in the background.
//! - Every new tunnel reads the remote's own identity before it serves a
//!   request; a different `machine_id` tears it down.
//! - A tunnel no request has used for [`TunnelConfig::idle_timeout`] is torn
//!   down by a timer thread that only compares instants.
//! - [`HostTunnels::shutdown_all`] stops every child on graceful shutdown and
//!   before a handover exec. Only children this process started are stopped:
//!   an attached remote dashboard keeps running.
//!
//! Everything here blocks (child processes, sleeps, blocking sockets) and is
//! called from the blocking pool, never from an async worker.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError, Weak};
use std::time::{Duration, Instant};

use orbit_common::{HostRegistryCode, OrbitError};
use serde::Serialize;
use serde_json::Value;
use tokio::sync::watch;

use crate::DEFAULT_DASHBOARD_PORT;
use crate::ssh_tunnel::{self, SshTunnel, TunnelOrigin, TunnelSpec, Unattended};

/// `ssh -o ConnectTimeout`: how long the TCP connect to a host may take.
const CONNECT_TIMEOUT_SECS: u64 = 10;

/// Largest identity response read through a new tunnel.
const IDENTITY_MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

/// How long [`HostTunnels::shutdown_all`] waits for an establish in progress
/// to notice the cancel flag and drop its child.
const SHUTDOWN_ESTABLISH_WAIT: Duration = Duration::from_secs(5);

/// Bounds and seams for the host forward. [`TunnelConfig::default`] is the
/// production configuration; a test shortens the timers and points
/// `ssh_program` at a stub with ssh's `-L` argv contract.
#[derive(Debug, Clone)]
pub(crate) struct TunnelConfig {
    /// `ssh`, resolved on `PATH`.
    pub(crate) ssh_program: String,
    /// The remote dashboard port the forward targets.
    pub(crate) remote_port: u16,
    /// Unused tunnels are torn down after this long.
    pub(crate) idle_timeout: Duration,
    /// Wait for an already-running remote dashboard once the forward binds.
    pub(crate) attach_timeout: Duration,
    /// Wait for a dashboard this process spawned.
    pub(crate) ready_timeout: Duration,
    /// Wait for `ssh` to bind the local forward (connect plus authentication).
    pub(crate) bind_timeout: Duration,
    /// Wait for the remote's identity row.
    pub(crate) identity_timeout: Duration,
}

impl Default for TunnelConfig {
    fn default() -> Self {
        Self {
            ssh_program: "ssh".to_string(),
            remote_port: DEFAULT_DASHBOARD_PORT,
            idle_timeout: Duration::from_secs(5 * 60),
            attach_timeout: Duration::from_secs(5),
            ready_timeout: Duration::from_secs(30),
            bind_timeout: Duration::from_secs(CONNECT_TIMEOUT_SECS + 10),
            identity_timeout: Duration::from_secs(10),
        }
    }
}

/// A registered remote host as the serving host's host file names it.
#[derive(Debug, Clone)]
pub(crate) struct HostTarget {
    /// The operator's name for the host (a legacy row's SSH target).
    pub(crate) name: String,
    /// The `machine_id` the entry records; the remote must report it.
    pub(crate) machine_id: String,
    /// The validated SSH target, only ever passed after `--`.
    pub(crate) ssh: String,
}

/// The remote's own row from `GET /api/hosts?probe=false`.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RemoteIdentity {
    pub(crate) machine_id: String,
    pub(crate) binary_version: Option<String>,
    pub(crate) protocol_fingerprint: Option<String>,
}

/// A typed forward failure in the host registry's vocabulary.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ForwardError {
    pub(crate) code: String,
    pub(crate) message: String,
}

impl ForwardError {
    fn unreachable(message: String) -> Self {
        Self {
            code: "unreachable_destination".to_string(),
            message,
        }
    }

    fn registry(code: HostRegistryCode, message: String) -> Self {
        Self {
            code: code.as_str().to_string(),
            message,
        }
    }

    /// An establish failure: a bound that ran out is `process_timeout`, and
    /// every other failure, including `classify_ssh_exit`'s reason, is
    /// `unreachable_destination`.
    fn from_establish(target: &HostTarget, error: &OrbitError) -> Self {
        let reason = match error {
            OrbitError::Execution(reason) | OrbitError::Io(reason) => reason.clone(),
            other => other.to_string(),
        };
        let message = format!("host '{}' is unreachable: {reason}", target.name);
        match error {
            OrbitError::ProcessTimeout { .. } => Self {
                code: "process_timeout".to_string(),
                message,
            },
            _ => Self::unreachable(message),
        }
    }

    fn shutting_down() -> Self {
        Self::unreachable("the dashboard is shutting down".to_string())
    }
}

/// One live forward and what was verified through it.
struct LiveTunnel {
    serial: u64,
    tunnel: SshTunnel,
    ssh: String,
    local_port: u16,
    origin: TunnelOrigin,
    identity: RemoteIdentity,
}

#[derive(Default)]
struct SlotState {
    live: Option<LiveTunnel>,
    last_error: Option<ForwardError>,
    /// Finished establish attempts. A request that waited behind an attempt
    /// takes that attempt's failure instead of starting another.
    attempts: u64,
    in_flight: usize,
    last_used: Option<Instant>,
}

/// One host's tunnel. `establish` is held for a whole establish (the
/// single-flight lock); `state` only ever for a few field reads and writes,
/// so a request finishing on an async worker never waits on an establish.
#[derive(Default)]
struct HostSlot {
    establish: Mutex<()>,
    state: Mutex<SlotState>,
}

/// A request's hold on a live tunnel. Counts as in-flight until dropped, so
/// the idle timer never tears down a tunnel under an open stream.
pub(crate) struct Lease {
    slot: Arc<HostSlot>,
    pub(crate) local_port: u16,
    pub(crate) origin: TunnelOrigin,
    pub(crate) identity: RemoteIdentity,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = lock(&self.slot.state);
        state.in_flight = state.in_flight.saturating_sub(1);
        state.last_used = Some(Instant::now());
    }
}

/// Every host tunnel this dashboard process owns.
pub(crate) struct HostTunnels {
    config: TunnelConfig,
    slots: Mutex<HashMap<String, Arc<HostSlot>>>,
    /// Shared with every establish as [`Unattended::cancel`]; also refuses new
    /// tunnels once shutdown begins.
    closing: Arc<AtomicBool>,
    /// Flipped to `true` once shutdown begins, so forwarded streams close.
    closing_signal: watch::Sender<bool>,
    serials: AtomicU64,
}

impl HostTunnels {
    pub(crate) fn new(config: TunnelConfig) -> Self {
        Self {
            config,
            slots: Mutex::new(HashMap::new()),
            closing: Arc::new(AtomicBool::new(false)),
            closing_signal: watch::Sender::new(false),
            serials: AtomicU64::new(0),
        }
    }

    pub(crate) fn remote_port(&self) -> u16 {
        self.config.remote_port
    }

    /// Resolves once shutdown begins. Forwarded streams select on it.
    pub(crate) fn closing(&self) -> watch::Receiver<bool> {
        self.closing_signal.subscribe()
    }

    /// A lease on `target`'s tunnel, establishing it when there is none or its
    /// child died. `operator` decides whether a spawned remote dashboard gets
    /// `--operator`; an attached one keeps its own capability.
    pub(crate) fn acquire(
        &self,
        target: &HostTarget,
        operator: bool,
    ) -> Result<Lease, ForwardError> {
        if self.closing.load(Ordering::Relaxed) {
            return Err(ForwardError::shutting_down());
        }
        let slot = self.slot(&target.machine_id);
        let seen = lock(&slot.state).attempts;
        if let Some(lease) = lease_live(&slot, target) {
            return Ok(lease);
        }
        let _single_flight = lock(&slot.establish);
        if let Some(lease) = lease_live(&slot, target) {
            return Ok(lease);
        }
        {
            let state = lock(&slot.state);
            if state.attempts != seen
                && let Some(error) = &state.last_error
            {
                return Err(error.clone());
            }
        }
        if self.closing.load(Ordering::Relaxed) {
            return Err(ForwardError::shutting_down());
        }
        let result = self.establish(target, operator);
        let mut state = lock(&slot.state);
        state.attempts += 1;
        let live = match result {
            Ok(live) => live,
            Err(error) => {
                state.last_error = Some(error.clone());
                return Err(error);
            }
        };
        if self.closing.load(Ordering::Relaxed) {
            drop(state);
            drop(live);
            return Err(ForwardError::shutting_down());
        }
        let serial = live.serial;
        let lease = Lease {
            slot: Arc::clone(&slot),
            local_port: live.local_port,
            origin: live.origin,
            identity: live.identity.clone(),
        };
        state.last_error = None;
        state.in_flight += 1;
        state.last_used = Some(Instant::now());
        state.live = Some(live);
        drop(state);
        watch_idle(Arc::downgrade(&slot), serial, self.config.idle_timeout);
        Ok(lease)
    }

    /// Refuse new tunnels and close forwarded streams. Live children keep
    /// serving in-flight requests until [`HostTunnels::shutdown_all`].
    pub(crate) fn begin_shutdown(&self) {
        self.closing.store(true, Ordering::Relaxed);
        self.closing_signal.send_replace(true);
    }

    /// Stop every child this process started: cancel establishes in progress,
    /// wait (bounded) for them to drop their child, then tear down every live
    /// tunnel. Idempotent.
    pub(crate) fn shutdown_all(&self) {
        self.begin_shutdown();
        let slots: Vec<Arc<HostSlot>> = lock(&self.slots).values().cloned().collect();
        let deadline = Instant::now() + SHUTDOWN_ESTABLISH_WAIT;
        for slot in slots {
            loop {
                match slot.establish.try_lock() {
                    Ok(_) | Err(TryLockError::Poisoned(_)) => break,
                    Err(TryLockError::WouldBlock) if Instant::now() >= deadline => {
                        tracing::warn!("a host tunnel establish did not stop before shutdown");
                        break;
                    }
                    Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(50)),
                }
            }
            let live = lock(&slot.state).live.take();
            drop(live);
        }
    }

    fn slot(&self, machine_id: &str) -> Arc<HostSlot> {
        Arc::clone(lock(&self.slots).entry(machine_id.to_string()).or_default())
    }

    fn establish(&self, target: &HostTarget, operator: bool) -> Result<LiveTunnel, ForwardError> {
        let local_port = ssh_tunnel::ephemeral_port()
            .map_err(|error| ForwardError::from_establish(target, &error))?;
        let spec = TunnelSpec {
            ssh_host: target.ssh.clone(),
            local_port,
            remote_port: self.config.remote_port,
            remote_command: remote_serve_command(operator, self.config.remote_port),
            remote_description: "orbit web serve".to_string(),
            readiness_target: format!("the dashboard on host '{}'", target.name),
            attach_timeout: self.config.attach_timeout,
            ready_timeout: self.config.ready_timeout,
            ssh_program: self.config.ssh_program.clone(),
            unattended: Some(Unattended {
                ssh_options: unattended_ssh_options(),
                bind_timeout: self.config.bind_timeout,
                cancel: Arc::clone(&self.closing),
            }),
        };
        // Every error return below drops `tunnel`, which stops the child.
        let (tunnel, origin) =
            ssh_tunnel::establish(&spec, || crate::connect::healthz_ok(local_port))
                .map_err(|error| ForwardError::from_establish(target, &error))?;
        let identity = read_identity(target, local_port, self.config.identity_timeout)?;
        if identity.machine_id != target.machine_id {
            return Err(ForwardError::registry(
                HostRegistryCode::HostIdentityMismatch,
                format!(
                    "host '{}' is registered as machine_id {} but the dashboard behind its \
                     tunnel reports {}; the tunnel was closed. If the host was reinstalled, \
                     remove and add it again",
                    target.name, target.machine_id, identity.machine_id
                ),
            ));
        }
        Ok(LiveTunnel {
            serial: self.serials.fetch_add(1, Ordering::Relaxed),
            tunnel,
            ssh: target.ssh.clone(),
            local_port,
            origin,
            identity,
        })
    }
}

/// `ssh` options for a server that has no terminal: never prompt (a
/// passphrase key goes through ssh-agent) and bound the TCP connect.
fn unattended_ssh_options() -> Vec<String> {
    vec![
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        "-o".to_string(),
        format!("ConnectTimeout={CONNECT_TIMEOUT_SECS}"),
    ]
}

/// The remote command for a spawned dashboard. `--operator` only when this
/// dashboard's session has it, so authority never rises across the forward.
pub(crate) fn remote_serve_command(operator: bool, remote_port: u16) -> String {
    let mut command = "orbit web serve --no-open".to_string();
    if operator {
        command.push_str(" --operator");
    }
    command.push_str(&format!(" --port {remote_port}"));
    command
}

/// A lease on the slot's live tunnel, or `None` after tearing down a tunnel
/// whose child exited or whose entry now names another SSH target.
fn lease_live(slot: &Arc<HostSlot>, target: &HostTarget) -> Option<Lease> {
    let mut state = lock(&slot.state);
    let usable = {
        let live = state.live.as_mut()?;
        live.ssh == target.ssh && matches!(live.tunnel.try_wait(), Ok(None))
    };
    if !usable {
        let stale = state.live.take();
        drop(state);
        drop(stale);
        return None;
    }
    let live = state.live.as_ref()?;
    let lease = Lease {
        slot: Arc::clone(slot),
        local_port: live.local_port,
        origin: live.origin,
        identity: live.identity.clone(),
    };
    state.in_flight += 1;
    state.last_used = Some(Instant::now());
    Some(lease)
}

/// Tear down tunnel `serial` once nothing has used it for `idle`. The thread
/// sleeps until the earliest possible idle instant, rechecks, and exits as
/// soon as the tunnel it watches is gone or replaced.
fn watch_idle(slot: Weak<HostSlot>, serial: u64, idle: Duration) {
    let spawned = std::thread::Builder::new()
        .name("orbit-web-host-tunnel-idle".to_string())
        .spawn(move || {
            loop {
                let Some(slot) = slot.upgrade() else {
                    return;
                };
                let wait = {
                    let mut state = lock(&slot.state);
                    if state.live.as_ref().map(|live| live.serial) != Some(serial) {
                        return;
                    }
                    let elapsed = state.last_used.map_or(idle, |used| used.elapsed());
                    if state.in_flight > 0 {
                        idle
                    } else if elapsed >= idle {
                        let expired = state.live.take();
                        drop(state);
                        drop(expired);
                        return;
                    } else {
                        idle - elapsed
                    }
                };
                drop(slot);
                std::thread::sleep(wait);
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not start the host tunnel idle timer; shutdown still stops it");
    }
}

/// Read the remote dashboard's own row from `GET /api/hosts?probe=false`.
/// A remote without the route predates host identity (`host_too_old`); any
/// other answer that does not name a local `machine_id` cannot be verified
/// and is refused as a mismatch.
fn read_identity(
    target: &HostTarget,
    local_port: u16,
    timeout: Duration,
) -> Result<RemoteIdentity, ForwardError> {
    let (status, body) =
        http_get(local_port, "/api/hosts?probe=false", timeout).map_err(|error| {
            ForwardError::unreachable(format!(
                "host '{}' did not answer its identity read: {error}",
                target.name
            ))
        })?;
    if status == 404 {
        return Err(ForwardError::registry(
            HostRegistryCode::HostTooOld,
            format!(
                "the dashboard on host '{}' has no /api/hosts, so its identity cannot be \
                 checked; upgrade Orbit on that host",
                target.name
            ),
        ));
    }
    let local_row = (status == 200)
        .then(|| serde_json::from_str::<Value>(&body).ok())
        .flatten()
        .and_then(|body| {
            body.get("hosts")?
                .as_array()?
                .iter()
                .find(|row| row.get("local").and_then(Value::as_bool) == Some(true))
                .cloned()
        });
    let text = |row: &Value, key: &str| row.get(key).and_then(Value::as_str).map(str::to_string);
    match local_row.as_ref().and_then(|row| text(row, "machine_id")) {
        Some(machine_id) => {
            let row = local_row.as_ref().unwrap_or(&Value::Null);
            Ok(RemoteIdentity {
                machine_id,
                binary_version: text(row, "binary_version"),
                protocol_fingerprint: text(row, "protocol_fingerprint"),
            })
        }
        None => Err(ForwardError::registry(
            HostRegistryCode::HostIdentityMismatch,
            format!(
                "the dashboard on host '{}' did not report its machine_id (HTTP {status}); \
                 the tunnel was closed",
                target.name
            ),
        )),
    }
}

/// A bounded HTTP/1.0 GET over the forward: the status and up to
/// [`IDENTITY_MAX_RESPONSE_BYTES`] of body.
fn http_get(local_port: u16, path: &str, timeout: Duration) -> std::io::Result<(u16, String)> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, local_port));
    let mut stream = TcpStream::connect_timeout(&addr, timeout)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let request = format!("GET {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes())?;
    let mut reader = BufReader::new(stream).take(IDENTITY_MAX_RESPONSE_BYTES);
    let mut status_line = String::new();
    reader.read_line(&mut status_line)?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .filter(|_| status_line.starts_with("HTTP/1."))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("not an HTTP response: {:?}", status_line.trim_end()),
            )
        })?;
    let mut response = String::new();
    reader.read_to_string(&mut response)?;
    let body = response
        .split_once("\r\n\r\n")
        .map_or(String::new(), |(_, body)| body.to_string());
    Ok((status, body))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
