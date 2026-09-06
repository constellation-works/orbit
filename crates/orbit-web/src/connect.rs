//! `orbit web connect <ssh-host>` — client-side SSH-tunnel convenience.
//!
//! The dashboard binds loopback-only by design (see [`crate::check_bindable_host`]):
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

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

use clap::Args;
use orbit_common::process::shell::quote_posix_arg;
use orbit_core::OrbitError;

use crate::ssh_tunnel::{self, SshTunnel, TunnelOrigin, TunnelSpec};
use crate::{DEFAULT_DASHBOARD_PORT, open_browser};

/// How long to wait for the remote dashboard to answer `/healthz` before
/// giving up. Generous because it covers SSH connect + remote process spawn.
const READINESS_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-probe TCP connect/read/write timeout for the `/healthz` check.
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long to wait, when first probing for an already-running remote
/// dashboard through a bare port forward, before concluding nothing is
/// listening and falling back to spawning one ourselves. Short: it only needs
/// to cover `ssh` handshake plus a couple of probe round trips, not a remote
/// process boot (that is what [`READINESS_TIMEOUT`] is for).
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

    /// Serve every workspace registered on the remote host, not just its
    /// default one — passes `--global` through to the remote `orbit web serve`.
    #[arg(long)]
    pub global: bool,

    /// Do not open the dashboard URL in a browser once the tunnel is ready.
    #[arg(long)]
    pub no_open: bool,
}

/// Establish the tunnel — attaching to an already-running remote dashboard
/// when one answers, spawning one otherwise — wait for readiness, open the
/// browser, and block until Ctrl-C / SIGTERM — then tear down only what this
/// invocation started.
///
/// A top-level `--root` is rejected rather than ignored: this command reads no
/// local `.orbit/` state, so there is no data directory for it to override,
/// and the flag used to name the *remote* workspace here (ORB-11388). See
/// [`reject_root_override`].
pub fn connect(args: ConnectArgs, root_override: Option<&Path>) -> Result<(), OrbitError> {
    reject_root_override(root_override)?;
    let local_port = select_local_port(args.port)?;

    // From here on, every exit path (error, panic, normal) tears the tunnel
    // down via `SshTunnel`'s `Drop`.
    let (mut tunnel, origin) =
        ssh_tunnel::establish(&tunnel_spec(&args, local_port), || healthz_ok(local_port))?;

    let url = format!("http://localhost:{local_port}");

    #[allow(clippy::print_stdout)]
    {
        match origin {
            TunnelOrigin::Attached => println!(
                "Attached to already-running remote dashboard: {url}  (Ctrl-C to disconnect)"
            ),
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
pub(crate) fn reject_root_override(root_override: Option<&Path>) -> Result<(), OrbitError> {
    match root_override {
        None => Ok(()),
        Some(_) => Err(OrbitError::InvalidInput(
            "orbit web connect does not accept --root; pass --workspace <SELECTOR> to choose the remote workspace"
                .to_string(),
        )),
    }
}

// Visibility note: the pure helpers below are `pub(crate)` so the sibling
// `tests/connect.rs` module can exercise them directly (the crate's test-layout
// convention). None are part of the crate's public API.

/// Describe this dashboard tunnel to the shared mechanism: which forward to
/// open, what to run remotely when nothing already answers, and how long to
/// wait for each of those two cases.
pub(crate) fn tunnel_spec(cfg: &ConnectArgs, local_port: u16) -> TunnelSpec {
    TunnelSpec {
        ssh_host: cfg.ssh_host.clone(),
        local_port,
        remote_port: cfg.remote_port,
        remote_command: remote_serve_command(cfg),
        remote_description: "orbit web serve".to_string(),
        readiness_target: format!("the remote dashboard at http://localhost:{local_port}/healthz"),
        attach_timeout: ATTACH_PROBE_TIMEOUT,
        ready_timeout: READINESS_TIMEOUT,
    }
}

/// Choose the local port to bind the tunnel to: an explicit `--port` is honored
/// or fails with a clear error if busy, otherwise the conventional
/// [`DEFAULT_DASHBOARD_PORT`] when free and an ephemeral port when it is not.
pub(crate) fn select_local_port(preferred: Option<u16>) -> Result<u16, OrbitError> {
    ssh_tunnel::select_local_port(preferred, DEFAULT_DASHBOARD_PORT)
}

/// The remote shell command line:
/// `orbit web serve --no-open --port N [--global] [--workspace S]`.
///
/// `ssh` concatenates trailing args with spaces and re-parses them via the
/// remote shell, so any value that could contain spaces (`--workspace`) is
/// shell-quoted.
///
/// ORB-11388: this forwards `--workspace`, not `--root`. On the remote,
/// `--root` now selects which registry is served (as it does for every other
/// command), so sending a workspace path there would serve an empty registry
/// instead of preselecting that workspace.
pub(crate) fn remote_serve_command(cfg: &ConnectArgs) -> String {
    let mut cmd = format!("orbit web serve --no-open --port {}", cfg.remote_port);
    if cfg.global {
        cmd.push_str(" --global");
    }
    if let Some(workspace) = &cfg.workspace {
        cmd.push_str(" --workspace ");
        cmd.push_str(&quote_posix_arg(workspace));
    }
    cmd
}

/// Best-effort `GET /healthz` over the forwarded local port. Returns `true`
/// only on a `200` status line. Any connect/IO error (including `ssh` refusing
/// the forwarded connection because the remote server is not up yet) is `false`.
fn healthz_ok(local_port: u16) -> bool {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, local_port));
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, PROBE_TIMEOUT) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));
    if stream
        .write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut status_line = String::new();
    if BufReader::new(stream).read_line(&mut status_line).is_err() {
        return false;
    }
    status_line.starts_with("HTTP/1.") && status_line.contains(" 200 ")
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
