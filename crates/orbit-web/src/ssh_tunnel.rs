//! Client-side SSH local-forward tunnel for `orbit web connect`.
//!
//! The web server is loopback-bound and has no authentication of its own.
//! Remote dashboard access therefore delegates authentication, encryption,
//! and host verification to SSH while keeping the HTTP listener private.
//!
//! Establishing is attach-first: a bare `-N` forward that
//! invokes nothing remotely is opened and probed, and only when nothing answers
//! behind it is a second `ssh` run that both forwards the port and starts the
//! remote command. OpenSSH binds the local `-L` listener only after
//! authentication, so the short attach budget starts at that bind — a
//! passphrase, password, or 2FA prompt does not expire it. Teardown therefore
//! only ever stops what this process started — an attached, pre-existing
//! remote server is never touched.
//!
//! Deliberately synchronous: the tunnel is a child-process lifetime, not a
//! future. The `connect` command owns the small async wait around it, and the
//! dashboard's host forward ([`crate::host_tunnels`]) runs it on the blocking
//! pool with [`Unattended`] bounds.

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use orbit_core::OrbitError;

/// Delay between readiness probes.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How long one "has `-L` bound yet?" connect may block.
///
/// Connection refused — the listener is not up yet — returns immediately.
/// This only bounds a black-holed route. It is not the attach budget.
const FORWARD_BIND_PROBE_TIMEOUT: Duration = Duration::from_millis(200);

/// Grace period between SIGTERM and SIGKILL when tearing the tunnel down.
#[cfg(unix)]
const TEARDOWN_GRACE: Duration = Duration::from_secs(2);

/// What [`establish`] needs to know to bring a forward up.
///
/// The remote command is the caller's, never composed here: this module owns
/// *how* a tunnel is opened and torn down, not *what* runs behind it.
#[derive(Debug, Clone)]
pub(crate) struct TunnelSpec {
    /// SSH destination — anything `ssh` accepts (`host`, `user@host`, or a
    /// `~/.ssh/config` alias).
    pub(crate) ssh_host: String,
    /// Local loopback port the forward binds.
    pub(crate) local_port: u16,
    /// Remote loopback port the forward targets.
    pub(crate) remote_port: u16,
    /// Shell command line run on the remote host when nothing already answers
    /// behind the forward. The caller must safely quote embedded values before
    /// constructing this command.
    pub(crate) remote_command: String,
    /// Human name for that command (`orbit web serve`), used in errors.
    pub(crate) remote_description: String,
    /// Human name for what readiness means ("the remote dashboard at
    /// http://localhost:7878/healthz"), used in the timeout error.
    pub(crate) readiness_target: String,
    /// How long to wait for an *already-running* remote server to answer
    /// through a bare forward *after the local listener accepts a connection*.
    /// Authentication is not included: OpenSSH binds `-L` only once the
    /// passphrase, password, or 2FA prompt has finished, and this budget must
    /// not expire while that prompt is still open.
    pub(crate) attach_timeout: Duration,
    /// How long to wait for a freshly spawned remote server to answer.
    /// Generous: it covers SSH connect plus remote process startup.
    pub(crate) ready_timeout: Duration,
    /// Executable that opens the tunnel. `orbit web connect` passes `ssh`,
    /// resolved on `PATH`. A test may pass a stub with the same argv contract.
    pub(crate) ssh_program: String,
    /// Bounds for an establish no terminal watches. `orbit web connect`
    /// passes `None`: its authentication wait is unbounded and Ctrl-C cancels
    /// it.
    pub(crate) unattended: Option<Unattended>,
}

/// How a server-side establish differs from the foreground `connect` one.
///
/// Nobody can answer a prompt or press Ctrl-C, so every wait is bounded,
/// a timeout is typed ([`OrbitError::ProcessTimeout`]), the caller can cancel
/// an establish in progress, and the remote command's stdout is discarded
/// (`ssh`'s own diagnostics still reach stderr).
#[derive(Debug, Clone)]
pub(crate) struct Unattended {
    /// `ssh` options placed before `-L`, such as `-o BatchMode=yes`.
    pub(crate) ssh_options: Vec<String>,
    /// Bound on the wait for `ssh` to bind the local forward, which covers
    /// TCP connect and authentication.
    pub(crate) bind_timeout: Duration,
    /// Set to stop an establish in progress; the next poll returns an error
    /// and drops (tears down) the child.
    pub(crate) cancel: Arc<AtomicBool>,
}

/// Whether [`establish`] attached to something already running or started it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TunnelOrigin {
    /// A server was already listening on the remote port; this process only
    /// opened a forward to it and must leave it running on teardown.
    Attached,
    /// Nothing answered, so this process started the remote command. Dropping
    /// the tunnel drops the connection, which SIGHUPs the remote pty session.
    Spawned,
}

/// Bring up a forward to `spec.remote_port`, attaching to an already-running
/// remote server when `ready` answers through a bare probe forward and starting
/// `spec.remote_command` only when nothing does.
///
/// `ready` is polled through the forward and decides readiness on its own; a
/// forward can come up healthy with nothing behind it, so `ssh`'s exit status
/// alone would not be enough (it still surfaces separately — see
/// [`classify_ssh_exit`] — when `ssh` itself fails, e.g. a bad host).
///
/// The attach budget does not include authentication. The probe stays up
/// until its local listener accepts a TCP connection (OpenSSH binds `-L`
/// only after auth) or the probe exits. Only then does [`TunnelSpec::attach_timeout`]
/// bound the `/healthz` wait.
///
/// The returned [`SshTunnel`] tears the forward down on drop, so every exit
/// path — error, panic, normal return — releases it.
pub(crate) fn establish(
    spec: &TunnelSpec,
    mut ready: impl FnMut() -> bool,
) -> Result<(SshTunnel, TunnelOrigin), OrbitError> {
    let unattended = spec.unattended.as_ref();
    let options = unattended.map_or(&[][..], |bounds| &bounds.ssh_options[..]);
    let mut probe = SshTunnel::new(spawn_ssh_for(
        spec,
        &probe_forward_args(&spec.ssh_host, spec.local_port, spec.remote_port, options),
    )?);
    // A refused connect here is "still authenticating", not "nothing listening".
    wait_until_forward_bound(&mut probe, spec)?;
    if poll_until_ready(
        &mut probe,
        &mut ready,
        spec.attach_timeout,
        &spec.remote_description,
        cancel_flag(spec),
    )? {
        return Ok((probe, TunnelOrigin::Attached));
    }
    probe.shutdown();

    let mut tunnel = SshTunnel::new(spawn_ssh_for(
        spec,
        &command_forward_args(
            &spec.ssh_host,
            spec.local_port,
            spec.remote_port,
            options,
            &spec.remote_command,
        ),
    )?);
    if poll_until_ready(
        &mut tunnel,
        &mut ready,
        spec.ready_timeout,
        &spec.remote_description,
        cancel_flag(spec),
    )? {
        return Ok((tunnel, TunnelOrigin::Spawned));
    }
    let detail = format!("waiting for {} to become ready", spec.readiness_target);
    Err(match unattended {
        Some(_) => OrbitError::ProcessTimeout {
            timeout_ms: duration_ms(spec.ready_timeout),
            detail,
        },
        None => OrbitError::Execution(format!(
            "timed out after {}s {detail}",
            spec.ready_timeout.as_secs(),
        )),
    })
}

fn cancel_flag(spec: &TunnelSpec) -> Option<&AtomicBool> {
    spec.unattended
        .as_ref()
        .map(|bounds| bounds.cancel.as_ref())
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The error an establish stopped through [`Unattended::cancel`] returns.
fn cancelled(remote_description: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "stopped establishing the tunnel to `{remote_description}`: the dashboard is shutting down"
    ))
}

/// Spawn `spec.ssh_program` (normally `ssh`) with the given argument vector.
/// `stdin` is null so Ctrl-C is delivered to *us* (the foreground process)
/// rather than being forwarded down a pty to the remote.
///
/// `stderr` is inherited so `ssh`'s own diagnostics (host key prompts, auth
/// failures) still reach the operator, or the server's log when unattended.
fn spawn_ssh_for(spec: &TunnelSpec, ssh_args: &[String]) -> Result<Child, OrbitError> {
    let mut command = Command::new(&spec.ssh_program);
    command.args(ssh_args).stdin(Stdio::null());
    if spec.unattended.is_some() {
        command.stdout(Stdio::null());
    }
    command
        .spawn()
        .map_err(|error| OrbitError::Io(format!("failed to launch {}: {error}", spec.ssh_program)))
}

/// Arguments for a bare probe forward: the port forward and nothing else
/// (`-N`, no trailing command).
///
/// Because it never invokes anything remotely, tearing it down on disconnect
/// cannot orphan or kill a pre-existing remote process; it only closes the
/// forward. That is what makes attaching safe.
pub(crate) fn probe_forward_args(
    ssh_host: &str,
    local_port: u16,
    remote_port: u16,
    options: &[String],
) -> Vec<String> {
    let mut args = vec![
        "-N".to_string(),
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
    ];
    args.extend_from_slice(options);
    args.extend([
        "-L".to_string(),
        forward_spec(local_port, remote_port),
        // `--` so a host beginning with `-` can never parse as an ssh option.
        "--".to_string(),
        ssh_host.to_string(),
    ]);
    args
}

/// Arguments for a forward that also runs `remote_command` on the far side.
///
/// `-tt` forces pty allocation even though stdin is null, so killing the local
/// `ssh` delivers SIGHUP to the remote pty and the remote command exits with
/// it — no orphan. `ExitOnForwardFailure` makes a port that cannot be forwarded
/// a startup failure rather than a remote command running with no tunnel.
pub(crate) fn command_forward_args(
    ssh_host: &str,
    local_port: u16,
    remote_port: u16,
    options: &[String],
    remote_command: &str,
) -> Vec<String> {
    let mut args = vec![
        "-tt".to_string(),
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
    ];
    args.extend_from_slice(options);
    args.extend([
        "-L".to_string(),
        forward_spec(local_port, remote_port),
        // `--` so a host beginning with `-` can never parse as an ssh option.
        "--".to_string(),
        ssh_host.to_string(),
        remote_command.to_string(),
    ]);
    args
}

/// The `-L` argument value binding both ends of the forward to loopback.
pub(crate) fn forward_spec(local_port: u16, remote_port: u16) -> String {
    format!("127.0.0.1:{local_port}:localhost:{remote_port}")
}

/// Return `Ok` if a loopback TCP listener can bind `port` (immediately released).
pub(crate) fn probe_bindable(port: u16) -> std::io::Result<()> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map(|_| ())
}

/// Ask the OS for a free ephemeral loopback port.
pub(crate) fn ephemeral_port() -> Result<u16, OrbitError> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|error| OrbitError::Io(format!("could not reserve a local port: {error}")))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|error| OrbitError::Io(format!("could not read reserved local port: {error}")))
}

/// Accept an operator-requested local port, failing with an actionable error
/// when it is already in use.
///
/// Note: inherently racy (TOCTOU) — the probed port can be claimed by another
/// process before `ssh` binds it. Acceptable because `ssh -L` then fails loudly
/// on startup rather than silently forwarding nothing.
pub(crate) fn require_local_port(port: u16) -> Result<u16, OrbitError> {
    probe_bindable(port).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "requested local port {port} is not available: {error}"
        ))
    })?;
    Ok(port)
}

/// Choose the local port to bind a forward to: an explicit request is honored
/// or fails, otherwise `preferred_default` is used when free and an ephemeral
/// port when it is not.
pub(crate) fn select_local_port(
    requested: Option<u16>,
    preferred_default: u16,
) -> Result<u16, OrbitError> {
    match requested {
        Some(port) => require_local_port(port),
        None if probe_bindable(preferred_default).is_ok() => Ok(preferred_default),
        None => ephemeral_port(),
    }
}

/// Wait until `ssh` has bound the local forward, or until it exits.
///
/// No deadline for `connect`: OpenSSH listens on `-L` only after
/// authentication, and a passphrase, password, or second-factor prompt must
/// not be cut off by [`TunnelSpec::attach_timeout`]. Ctrl-C cancels a stuck
/// prompt; it is delivered here because the child's stdin is null. An
/// [`Unattended`] establish has no prompt to wait for, so it is bounded by
/// [`Unattended::bind_timeout`] and stops when cancelled. An exit before the
/// listener accepts is a connection failure, not "nothing is running".
fn wait_until_forward_bound(tunnel: &mut SshTunnel, spec: &TunnelSpec) -> Result<(), OrbitError> {
    let deadline = spec
        .unattended
        .as_ref()
        .map(|bounds| (Instant::now() + bounds.bind_timeout, bounds));
    loop {
        if let Some(status) = tunnel.try_wait()? {
            return Err(classify_ssh_exit(status, &spec.remote_description));
        }
        if forward_listener_up(spec.local_port) {
            return Ok(());
        }
        if let Some((deadline, bounds)) = deadline {
            if bounds.cancel.load(Ordering::Relaxed) {
                return Err(cancelled(&spec.remote_description));
            }
            if Instant::now() >= deadline {
                return Err(OrbitError::ProcessTimeout {
                    timeout_ms: duration_ms(bounds.bind_timeout),
                    detail: format!(
                        "ssh to {} did not open the local forward (connect and authentication)",
                        spec.ssh_host
                    ),
                });
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// True when a TCP handshake to the loopback forward port completes.
///
/// A completed handshake means the local listener is bound. It does not mean
/// the far side accepted the forwarded channel.
fn forward_listener_up(local_port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, local_port));
    TcpStream::connect_timeout(&addr, FORWARD_BIND_PROBE_TIMEOUT).is_ok()
}

/// Poll `ready` through `tunnel`'s forward until it answers or `timeout`
/// elapses.
///
/// Returns `Ok(true)` once ready, `Ok(false)` on a plain timeout (the forward
/// is still up; nothing has answered yet), or `Err` if `ssh` exited before
/// either happened — a dead `ssh` is a configuration failure, not a
/// "nothing running there yet" — or `cancel` was set.
pub(crate) fn poll_until_ready(
    tunnel: &mut SshTunnel,
    mut ready: impl FnMut() -> bool,
    timeout: Duration,
    remote_description: &str,
    cancel: Option<&AtomicBool>,
) -> Result<bool, OrbitError> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = tunnel.try_wait()? {
            return Err(classify_ssh_exit(status, remote_description));
        }
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            return Err(cancelled(remote_description));
        }
        if ready() {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Map an early `ssh` exit to an actionable error.
pub(crate) fn classify_ssh_exit(status: ExitStatus, remote_description: &str) -> OrbitError {
    match status.code() {
        // The remote shell returns 127 when it cannot find the command.
        Some(127) => OrbitError::Execution(
            "`orbit` was not found on the remote host's PATH (ssh exited 127). \
             Ensure orbit is installed and on PATH for non-interactive SSH \
             sessions (e.g. add it to ~/.profile / ~/.bashrc on the remote)."
                .to_string(),
        ),
        // ssh's own failure code (bad host, auth, network).
        Some(255) => OrbitError::Execution(
            "ssh could not connect (exit 255). Check the host, your SSH \
             config/keys, and network reachability."
                .to_string(),
        ),
        Some(code) => OrbitError::Execution(format!(
            "remote `{remote_description}` exited with status {code} before it became ready"
        )),
        None => OrbitError::Execution(format!(
            "ssh was terminated by a signal before `{remote_description}` became ready"
        )),
    }
}

/// RAII owner of the `ssh` child that guarantees teardown of the forward on
/// drop.
///
/// When this invocation spawned a remote command ([`TunnelOrigin::Spawned`]),
/// closing `ssh` also delivers SIGHUP to the remote pty and stops that process.
/// When it only attached via a bare `-N` forward ([`TunnelOrigin::Attached`]),
/// there is no remote command tied to this session, so teardown just closes the
/// forward and leaves the pre-existing remote process running.
pub(crate) struct SshTunnel {
    child: Option<Child>,
}

impl SshTunnel {
    pub(crate) fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    /// Non-blocking check for the child's exit status.
    pub(crate) fn try_wait(&mut self) -> Result<Option<ExitStatus>, OrbitError> {
        match &mut self.child {
            Some(child) => child
                .try_wait()
                .map_err(|error| OrbitError::Io(format!("waiting on ssh: {error}"))),
            None => Ok(None),
        }
    }

    /// Terminate the `ssh` child if it is still running. Idempotent.
    pub(crate) fn shutdown(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if let Ok(Some(_)) = child.try_wait() {
            return; // already gone
        }
        terminate_child(&mut child);
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Ask the child to exit gracefully (SIGTERM), then force it (SIGKILL) if it
/// does not within [`TEARDOWN_GRACE`].
#[cfg(unix)]
pub(crate) fn terminate_child(child: &mut Child) {
    let pid = child.id() as libc::pid_t;
    // SAFETY: `pid` is our own direct child; signalling it is well-defined.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + TEARDOWN_GRACE;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(unix))]
pub(crate) fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}
