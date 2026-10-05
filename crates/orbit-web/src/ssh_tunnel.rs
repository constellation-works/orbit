//! Client-side SSH local-forward tunnel for `orbit web connect`.
//!
//! The web server is loopback-bound and has no authentication of its own.
//! Remote dashboard access therefore delegates authentication, encryption,
//! and host verification to SSH while keeping the HTTP listener private.
//!
//! Establishing is attach-first: a bare `-N` forward that
//! invokes nothing remotely is opened and probed, and only when nothing answers
//! behind it is a second `ssh` run that both forwards the port and starts the
//! remote command. Teardown therefore only ever stops what this process
//! started — an attached, pre-existing remote server is never touched.
//!
//! Deliberately synchronous: the tunnel is a child-process lifetime, not a
//! future. The `connect` command owns the small async wait around it.

use std::net::{Ipv4Addr, TcpListener};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use orbit_core::OrbitError;

/// Delay between readiness probes.
pub(crate) const POLL_INTERVAL: Duration = Duration::from_millis(250);

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
    /// through a bare forward before concluding nothing is listening. Short:
    /// it covers an SSH handshake plus a couple of probes, not a process boot.
    pub(crate) attach_timeout: Duration,
    /// How long to wait for a freshly spawned remote server to answer.
    /// Generous: it covers SSH connect plus remote process startup.
    pub(crate) ready_timeout: Duration,
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
/// The returned [`SshTunnel`] tears the forward down on drop, so every exit
/// path — error, panic, normal return — releases it.
pub(crate) fn establish(
    spec: &TunnelSpec,
    mut ready: impl FnMut() -> bool,
) -> Result<(SshTunnel, TunnelOrigin), OrbitError> {
    let mut probe = SshTunnel::new(spawn_ssh(&probe_forward_args(
        &spec.ssh_host,
        spec.local_port,
        spec.remote_port,
    ))?);
    if poll_until_ready(
        &mut probe,
        &mut ready,
        spec.attach_timeout,
        &spec.remote_description,
    )? {
        return Ok((probe, TunnelOrigin::Attached));
    }
    probe.shutdown();

    let mut tunnel = SshTunnel::new(spawn_ssh(&command_forward_args(
        &spec.ssh_host,
        spec.local_port,
        spec.remote_port,
        &spec.remote_command,
    ))?);
    if poll_until_ready(
        &mut tunnel,
        &mut ready,
        spec.ready_timeout,
        &spec.remote_description,
    )? {
        Ok((tunnel, TunnelOrigin::Spawned))
    } else {
        Err(OrbitError::Execution(format!(
            "timed out after {}s waiting for {} to become ready",
            spec.ready_timeout.as_secs(),
            spec.readiness_target
        )))
    }
}

/// Spawn `ssh` with the given argument vector. `stdin` is null so Ctrl-C is
/// delivered to *us* (the foreground process) rather than being forwarded down
/// a pty to the remote.
///
/// `stdout`/`stderr` are inherited so `ssh`'s own diagnostics (host key
/// prompts, auth failures) still reach the operator.
pub(crate) fn spawn_ssh(ssh_args: &[String]) -> Result<Child, OrbitError> {
    Command::new("ssh")
        .args(ssh_args)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|error| OrbitError::Io(format!("failed to launch ssh: {error}")))
}

/// Arguments for a bare probe forward: the port forward and nothing else
/// (`-N`, no trailing command).
///
/// Because it never invokes anything remotely, tearing it down on disconnect
/// cannot orphan or kill a pre-existing remote process; it only closes the
/// forward. That is what makes attaching safe.
pub(crate) fn probe_forward_args(ssh_host: &str, local_port: u16, remote_port: u16) -> Vec<String> {
    vec![
        "-N".to_string(),
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
        "-L".to_string(),
        forward_spec(local_port, remote_port),
        // `--` so a host beginning with `-` can never parse as an ssh option.
        "--".to_string(),
        ssh_host.to_string(),
    ]
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
    remote_command: &str,
) -> Vec<String> {
    vec![
        "-tt".to_string(),
        "-o".to_string(),
        "ExitOnForwardFailure=yes".to_string(),
        "-L".to_string(),
        forward_spec(local_port, remote_port),
        // `--` so a host beginning with `-` can never parse as an ssh option.
        "--".to_string(),
        ssh_host.to_string(),
        remote_command.to_string(),
    ]
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

/// Poll `ready` through `tunnel`'s forward until it answers or `timeout`
/// elapses.
///
/// Returns `Ok(true)` once ready, `Ok(false)` on a plain timeout (the forward
/// is still up; nothing has answered yet), or `Err` if `ssh` exited before
/// either happened — a dead `ssh` is a configuration failure, not a
/// "nothing running there yet".
pub(crate) fn poll_until_ready(
    tunnel: &mut SshTunnel,
    mut ready: impl FnMut() -> bool,
    timeout: Duration,
    remote_description: &str,
) -> Result<bool, OrbitError> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = tunnel.try_wait()? {
            return Err(classify_ssh_exit(status, remote_description));
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
