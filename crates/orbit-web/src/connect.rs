//! `orbit web connect <ssh-host>` — client-side SSH-tunnel convenience.
//!
//! The dashboard binds loopback-only by design (see [`crate::serve::check_bindable_host`]):
//! it has no authentication, so it must never be exposed to a network directly.
//! To view a workspace's dashboard from another machine the supported path is
//! an authenticated SSH tunnel — historically the manual
//! `ssh -L 7878:localhost:7878 <host> "orbit web serve --no-open"`.
//!
//! `connect` automates exactly that workflow and nothing more: it delegates
//! authentication to SSH, keeps the loopback bind guard intact, and adds no new
//! attack surface. The attach-or-spawn process lifecycle lives beside this
//! command in [`crate::ssh_tunnel`]; this module adds the `/healthz` readiness
//! probe, the remote `orbit web serve` command line, the browser, and the
//! shutdown wait.
//!
//! Either way it waits for the remote server to answer `/healthz`, opens a
//! browser, and — on Ctrl-C — tears down only the `ssh` process this invocation
//! started: a spawned remote `orbit web serve` is never orphaned, and an
//! attached, pre-existing one is never touched.
//!
//! Unlike [`crate::serve`], this command reads no local `.orbit/` state: the
//! workspace lives on the remote, so it needs no [`orbit_core::OrbitRuntime`].

use std::io::{self, ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use clap::Args;
use orbit_common::process::shell::quote_posix_arg;
use orbit_core::OrbitError;

use crate::ssh_tunnel::{self, ProbeLimit, SshTunnel, TunnelOrigin, TunnelSpec};
use crate::{DEFAULT_DASHBOARD_PORT, open_browser};

/// How long to wait for the remote dashboard to answer `/healthz` before
/// giving up. Generous because it covers SSH connect + remote process spawn.
const READINESS_TIMEOUT: Duration = Duration::from_secs(30);

/// How long one idle socket read or write may block. A peer that keeps sending
/// bytes resets this, so it is not the probe's deadline; [`ProbeLimit`] and
/// [`PROBE_EXCHANGE_TIMEOUT`] are.
pub(crate) const PROBE_IO_TIMEOUT: Duration = Duration::from_millis(500);

/// Status line cap. A line with no newline by this size is refused rather than
/// appended to without a bound.
pub(crate) const PROBE_MAX_STATUS_LINE_BYTES: usize = 8 * 1024;

/// Header block after the status line, same refusal rule.
pub(crate) const PROBE_MAX_HEADER_BYTES: usize = 8 * 1024;

/// Largest probe body. Hitting the cap before EOF refuses the response.
const PROBE_MAX_BODY_BYTES: usize = 1024 * 1024;

/// Total time for one post-attach body probe (`GET /api/routines`). A trickle
/// faster than [`PROBE_IO_TIMEOUT`] cannot extend it.
const PROBE_EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait, once the local forward is accepting connections, for an
/// already-running remote dashboard to answer `/healthz` before concluding
/// nothing is listening and spawning one. Authentication is not included:
/// OpenSSH binds `-L` only after the passphrase, password, or 2FA prompt
/// finishes, and that time must not consume this budget (a remote process
/// boot is what [`READINESS_TIMEOUT`] is for).
const ATTACH_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Arguments for `orbit web connect`.
#[derive(Args, Clone)]
#[command(about = "Open a remote workspace's dashboard over an SSH tunnel")]
pub struct ConnectArgs {
    /// SSH destination — anything `ssh` accepts (`host`, `user@host`, or a
    /// `~/.ssh/config` alias).
    pub ssh_host: String,

    /// Local port for the tunnel. Defaults to 7878, falling back to an
    /// ephemeral port if 7878 is already in use.
    #[arg(long)]
    pub port: Option<u16>,

    /// Port the remote `orbit web serve` listens on (remote loopback).
    #[arg(long, default_value_t = DEFAULT_DASHBOARD_PORT)]
    pub remote_port: u16,

    /// Remote workspace to preselect — a registered name, logical ID (`ws_*`),
    /// or remote checkout path — passed through to `orbit web serve
    /// --workspace` on the remote host. Omit to use the remote's default
    /// workspace resolution.
    #[arg(long, value_name = "SELECTOR")]
    pub workspace: Option<String>,

    /// Deprecated, no-op: remote dashboards always serve every registered
    /// workspace. Forwarded when spawning a dashboard for compatibility with
    /// older remote binaries.
    #[arg(long)]
    pub global: bool,

    /// Do not open the dashboard URL in a browser once the tunnel is ready.
    #[arg(long)]
    pub no_open: bool,

    /// Spawn the remote dashboard without operator capability. Operations
    /// controls stay read-only. The default is to pass `--operator` because
    /// the SSH login is the operator act.
    #[arg(long)]
    pub no_operator: bool,
}

/// Establish the tunnel — attaching to an already-running remote dashboard
/// when one answers, spawning one otherwise — wait for readiness, open the
/// browser, and block until Ctrl-C / SIGTERM — then tear down only what this
/// invocation started.
///
/// A top-level `--root` is rejected rather than ignored: this command reads no
/// local `.orbit/` state, so there is no data directory for it to override,
/// and the flag previously used to name the *remote* workspace is now
/// `--workspace`.
pub fn connect(args: ConnectArgs, root_override: Option<&Path>) -> Result<(), OrbitError> {
    reject_root_override(root_override)?;
    let local_port = select_local_port(args.port)?;

    // From here on, every exit path (error, panic, normal) tears the tunnel
    // down via `SshTunnel`'s `Drop`.
    let (mut tunnel, origin) = ssh_tunnel::establish(&tunnel_spec(&args, local_port), |limit| {
        healthz_ok(local_port, limit)
    })?;

    let url = format!("http://localhost:{local_port}");

    #[allow(clippy::print_stdout)]
    {
        match origin {
            TunnelOrigin::Attached => {
                println!(
                    "Attached to already-running remote dashboard: {url}  (Ctrl-C to disconnect)"
                );
                if let Some(notice) =
                    attached_without_operator_notice(routines_controls_authorized(local_port))
                {
                    println!("{notice}");
                }
            }
            TunnelOrigin::Spawned => {
                println!("Dashboard tunnel ready: {url}  (Ctrl-C to disconnect)")
            }
        }
    }

    if !args.no_open {
        open_browser(&url);
    }

    wait_for_shutdown(&mut tunnel);
    Ok(())
}

/// `orbit web connect` overrides no local data directory, so `--root` can only
/// be a mistake here. Before ORB-11388 this command's own `--root` named the
/// remote workspace to preselect; that is `--workspace` now, and an invocation
/// still passing `--root` reaches the top-level flag instead. Failing with
/// that redirection is the migration: silently ignoring it would drop the
/// preselection without a word.
fn reject_root_override(root_override: Option<&Path>) -> Result<(), OrbitError> {
    match root_override {
        None => Ok(()),
        Some(_) => Err(OrbitError::InvalidInput(
            "orbit web connect does not accept --root; pass --workspace <SELECTOR> to choose the remote workspace"
                .to_string(),
        )),
    }
}

/// Describe this dashboard tunnel to the shared mechanism: which forward to
/// open, what to run remotely when nothing already answers, and how long to
/// wait for each of those two cases.
fn tunnel_spec(cfg: &ConnectArgs, local_port: u16) -> TunnelSpec {
    TunnelSpec {
        ssh_host: cfg.ssh_host.clone(),
        local_port,
        remote_port: cfg.remote_port,
        remote_command: remote_serve_command(cfg),
        remote_description: "orbit web serve".to_string(),
        readiness_target: format!("the remote dashboard at http://localhost:{local_port}/healthz"),
        attach_timeout: ATTACH_PROBE_TIMEOUT,
        ready_timeout: READINESS_TIMEOUT,
        ssh_program: "ssh".to_string(),
        unattended: None,
    }
}

/// Choose the local port to bind the tunnel to: an explicit `--port` is honored
/// or fails with a clear error if busy, otherwise the conventional
/// [`DEFAULT_DASHBOARD_PORT`] when free and an ephemeral port when it is not.
fn select_local_port(preferred: Option<u16>) -> Result<u16, OrbitError> {
    ssh_tunnel::select_local_port(preferred, DEFAULT_DASHBOARD_PORT)
}

/// The remote shell command line:
/// `orbit web serve --no-open --operator --port N [--global] [--workspace S]`.
/// `--operator` is omitted when `--no-operator` was passed.
///
/// `ssh` concatenates trailing args with spaces and re-parses them via the
/// remote shell, so any value that could contain spaces (`--workspace`) is
/// shell-quoted.
///
/// ORB-11388: this forwards `--workspace`, not `--root`. On the remote,
/// `--root` now selects which registry is served (as it does for every other
/// command), so sending a workspace path there would serve an empty registry
/// instead of preselecting that workspace.
fn remote_serve_command(cfg: &ConnectArgs) -> String {
    let mut cmd = "orbit web serve --no-open".to_string();
    if !cfg.no_operator {
        cmd.push_str(" --operator");
    }
    cmd.push_str(&format!(" --port {}", cfg.remote_port));
    if cfg.global {
        cmd.push_str(" --global");
    }
    if let Some(workspace) = &cfg.workspace {
        cmd.push_str(" --workspace ");
        cmd.push_str(&quote_posix_arg(workspace));
    }
    cmd
}

/// Notice printed when this invocation attached to a pre-existing remote that
/// is not an operator session. The remote was not started here, so it cannot
/// be upgraded in place.
fn attached_without_operator_notice(controls_authorized: Option<bool>) -> Option<&'static str> {
    match controls_authorized {
        Some(false) => Some(
            "This remote dashboard is already running without operator capability. \
             Operations controls stay read-only; this invocation cannot upgrade it in place. \
             Restart the remote process with `orbit web serve --operator`, or stop it and \
             reconnect so this command can spawn one.",
        ),
        _ => None,
    }
}

/// `controls_authorized` from a `/api/routines` JSON body, if present.
fn parse_controls_authorized(body: &str) -> Option<bool> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("controls_authorized")?
        .as_bool()
}

/// Best-effort `GET /api/routines` over the forwarded local port. Used only
/// to decide whether an attached remote already has operator capability.
fn routines_controls_authorized(local_port: u16) -> Option<bool> {
    parse_controls_authorized(&http_get_ok_body(local_port, "/api/routines")?)
}

/// Whether an idle read ends the attempt or keeps waiting until the deadline.
///
/// Readiness polling uses [`ProbeIdle::Yield`]: a silent peer must return so
/// the next poll can run. A one-shot body read uses
/// [`ProbeIdle::UntilDeadline`]: gaps shorter than the total budget still
/// succeed, and a trickle cannot extend that budget.
#[derive(Clone, Copy)]
pub(crate) enum ProbeIdle {
    /// Return when one read waits out [`PROBE_IO_TIMEOUT`] with no bytes.
    Yield,
    /// Keep reading through idle gaps until the caller's deadline.
    UntilDeadline,
}

/// Status and body of one bounded HTTP/1.0 GET.
pub(crate) struct ProbedHttp {
    pub(crate) status: u16,
    pub(crate) body: String,
}

/// `GET path` over the forwarded local port. Returns the response body only
/// on a `200` status line. The exchange stops at [`PROBE_EXCHANGE_TIMEOUT`]
/// even when the peer keeps sending.
fn http_get_ok_body(local_port: u16, path: &str) -> Option<String> {
    let deadline = Instant::now() + PROBE_EXCHANGE_TIMEOUT;
    let response = probe_http_get(
        local_port,
        path,
        deadline,
        None,
        PROBE_MAX_BODY_BYTES,
        ProbeIdle::UntilDeadline,
    )
    .ok()?;
    (response.status == 200).then_some(response.body)
}

/// `GET path`, bounded by `deadline` and `max_body_bytes`.
///
/// The status line and the header block each stop at their own caps. An
/// oversized line, a body that reaches `max_body_bytes` before EOF, or a
/// response that is still incomplete at `deadline` is an error. `cancel` ends
/// the read between bytes.
pub(crate) fn probe_http_get(
    local_port: u16,
    path: &str,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
    max_body_bytes: usize,
    idle: ProbeIdle,
) -> io::Result<ProbedHttp> {
    let mut stream = connect_probed(local_port, deadline, cancel)?;
    let request = format!("GET {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    write_all_bounded(&mut stream, request.as_bytes(), deadline, cancel)?;
    let status_line = read_status_line(&mut stream, deadline, cancel, idle)?;
    let status = http_status_code(&status_line)?;
    read_header_block(&mut stream, deadline, cancel, idle)?;
    let body = read_capped_body(&mut stream, max_body_bytes, deadline, cancel, idle)?;
    let body = String::from_utf8(body)
        .map_err(|_| io::Error::new(ErrorKind::InvalidData, "tunnel probe body is not utf-8"))?;
    Ok(ProbedHttp { status, body })
}

/// Best-effort `GET /healthz` over the forwarded local port. Returns `true`
/// only on a `200` status line. Any connect/IO error (including `ssh` refusing
/// the forwarded connection because the remote server is not up yet) is `false`.
///
/// Only the status line is read. `limit` is the attach or spawn budget still
/// remaining: a peer that trickles bytes without a newline cannot outlive it,
/// and a line longer than [`PROBE_MAX_STATUS_LINE_BYTES`] is not healthy.
pub(crate) fn healthz_ok(local_port: u16, limit: ProbeLimit<'_>) -> bool {
    let deadline = limit.deadline;
    let cancel = limit.cancel;
    if probe_stopped(deadline, cancel) {
        return false;
    }
    let Ok(mut stream) = connect_probed(local_port, deadline, cancel) else {
        return false;
    };
    if write_all_bounded(
        &mut stream,
        b"GET /healthz HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        deadline,
        cancel,
    )
    .is_err()
    {
        return false;
    }
    // Yield on an idle read so a port with nothing behind it is retried inside
    // the same budget. A trickle still hits `deadline` between bytes.
    let Ok(status_line) = read_status_line(&mut stream, deadline, cancel, ProbeIdle::Yield) else {
        return false;
    };
    status_line.starts_with("HTTP/1.") && status_line.contains(" 200 ")
}

fn connect_probed(
    port: u16,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
) -> io::Result<TcpStream> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    loop {
        if cancel_requested(cancel) {
            return Err(cancelled_io());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(deadline_io());
        }
        match TcpStream::connect_timeout(&addr, remaining.min(PROBE_IO_TIMEOUT)) {
            Ok(stream) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

fn write_all_bounded(
    stream: &mut TcpStream,
    bytes: &[u8],
    deadline: Instant,
    cancel: Option<&AtomicBool>,
) -> io::Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        if cancel_requested(cancel) {
            return Err(cancelled_io());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(deadline_io());
        }
        stream.set_write_timeout(Some(remaining.min(PROBE_IO_TIMEOUT)))?;
        match stream.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    ErrorKind::WriteZero,
                    "tunnel probe write stalled",
                ));
            }
            Ok(written) => offset += written,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if is_idle(&error) => return Err(deadline_io()),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_status_line(
    stream: &mut TcpStream,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
    idle: ProbeIdle,
) -> io::Result<String> {
    let bytes = read_capped_line(stream, PROBE_MAX_STATUS_LINE_BYTES, deadline, cancel, idle)?;
    let line = std::str::from_utf8(&bytes).map_err(|_| {
        io::Error::new(
            ErrorKind::InvalidData,
            "tunnel probe status line is not utf-8",
        )
    })?;
    Ok(line.to_string())
}

fn read_header_block(
    stream: &mut TcpStream,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
    idle: ProbeIdle,
) -> io::Result<()> {
    let mut total = 0usize;
    loop {
        if total >= PROBE_MAX_HEADER_BYTES {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                format!("tunnel probe headers exceed {PROBE_MAX_HEADER_BYTES} bytes"),
            ));
        }
        let line = read_capped_line(
            stream,
            PROBE_MAX_HEADER_BYTES - total,
            deadline,
            cancel,
            idle,
        )?;
        total += line.len();
        if line.as_slice() == b"\n" || line.as_slice() == b"\r\n" {
            return Ok(());
        }
    }
}

/// Read one line, including its trailing `\n`, refusing to grow past `max_bytes`.
fn read_capped_line(
    stream: &mut TcpStream,
    max_bytes: usize,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
    idle: ProbeIdle,
) -> io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(max_bytes.min(128));
    let mut byte = [0u8; 1];
    loop {
        if cancel_requested(cancel) {
            return Err(cancelled_io());
        }
        if Instant::now() >= deadline {
            return Err(deadline_io());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        stream.set_read_timeout(Some(remaining.min(PROBE_IO_TIMEOUT)))?;
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "tunnel probe response ended before a complete line",
                ));
            }
            Ok(_) => {
                if buf.len() >= max_bytes {
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        format!("tunnel probe line exceeds {max_bytes} bytes"),
                    ));
                }
                buf.push(byte[0]);
                if byte[0] == b'\n' {
                    return Ok(buf);
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if is_idle(&error) => match idle {
                ProbeIdle::Yield => {
                    return Err(io::Error::new(
                        ErrorKind::TimedOut,
                        "tunnel probe read idle",
                    ));
                }
                ProbeIdle::UntilDeadline => continue,
            },
            Err(error) => return Err(error),
        }
    }
}

fn read_capped_body(
    stream: &mut TcpStream,
    max_bytes: usize,
    deadline: Instant,
    cancel: Option<&AtomicBool>,
    idle: ProbeIdle,
) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8 * 1024];
    loop {
        if cancel_requested(cancel) {
            return Err(cancelled_io());
        }
        if Instant::now() >= deadline {
            return Err(deadline_io());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        stream.set_read_timeout(Some(remaining.min(PROBE_IO_TIMEOUT)))?;
        if buf.len() >= max_bytes {
            let mut extra = [0u8; 1];
            match stream.read(&mut extra) {
                Ok(0) => return Ok(buf),
                Ok(_) => {
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        format!("tunnel probe body exceeds {max_bytes} bytes"),
                    ));
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) if is_idle(&error) => match idle {
                    ProbeIdle::Yield => {
                        return Err(io::Error::new(
                            ErrorKind::TimedOut,
                            "tunnel probe read idle",
                        ));
                    }
                    ProbeIdle::UntilDeadline => continue,
                },
                Err(error) => return Err(error),
            }
        }
        let room = max_bytes - buf.len();
        let want = room.min(chunk.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) => return Ok(buf),
            Ok(read) => buf.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if is_idle(&error) => match idle {
                ProbeIdle::Yield => {
                    return Err(io::Error::new(
                        ErrorKind::TimedOut,
                        "tunnel probe read idle",
                    ));
                }
                ProbeIdle::UntilDeadline => continue,
            },
            Err(error) => return Err(error),
        }
    }
}

fn http_status_code(status_line: &str) -> io::Result<u16> {
    if !status_line.starts_with("HTTP/1.") {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("not an HTTP response: {:?}", status_line.trim_end()),
        ));
    }
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| {
            io::Error::new(
                ErrorKind::InvalidData,
                format!("not an HTTP response: {:?}", status_line.trim_end()),
            )
        })
}

fn probe_stopped(deadline: Instant, cancel: Option<&AtomicBool>) -> bool {
    cancel_requested(cancel) || Instant::now() >= deadline
}

fn cancel_requested(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed))
}

fn is_idle(error: &io::Error) -> bool {
    matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock)
}

fn deadline_io() -> io::Error {
    io::Error::new(ErrorKind::TimedOut, "tunnel probe deadline exceeded")
}

fn cancelled_io() -> io::Error {
    io::Error::new(ErrorKind::Interrupted, "tunnel probe cancelled")
}

/// Block until Ctrl-C / SIGTERM, or until the `ssh` child exits on its own
/// (e.g. the remote server dies). Teardown then happens via `SshTunnel::Drop`.
///
/// Uses a small current-thread tokio runtime and the same signal primitives as
/// [`crate::serve`] so behavior is consistent across the two `web` surfaces. If
/// the runtime cannot be built we fall back to a plain poll loop that at least
/// returns when the child exits (teardown still runs on drop; a Ctrl-C in that
/// degraded mode terminates the process, and the OS reaps the tunnel).
fn wait_for_shutdown(tunnel: &mut SshTunnel) {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        while !matches!(tunnel.try_wait(), Ok(Some(_))) {
            std::thread::sleep(ssh_tunnel::POLL_INTERVAL);
        }
        return;
    };

    rt.block_on(async {
        let ctrl_c = async {
            let _ = tokio::signal::ctrl_c().await;
        };

        #[cfg(unix)]
        let terminate = async {
            if let Ok(mut sig) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                sig.recv().await;
            }
        };
        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        // Also return if `ssh` dies on its own (remote server crashed, network
        // dropped, …) so we do not hang forever.
        let child_exit = async {
            while !matches!(tunnel.try_wait(), Ok(Some(_))) {
                tokio::time::sleep(ssh_tunnel::POLL_INTERVAL).await;
            }
        };

        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate => {}
            _ = child_exit => {}
        }
    });
}
