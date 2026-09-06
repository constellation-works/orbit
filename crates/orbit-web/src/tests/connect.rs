//! Unit tests for `orbit web connect` helpers.
//!
//! Only what this command owns is tested here: the remote `orbit web serve`
//! command line, how that is wired into a [`crate::ssh_tunnel::TunnelSpec`],
//! and local port selection. The web-owned tunnel process lifecycle is tested
//! separately in the sibling `tests/ssh_tunnel.rs` module.

use std::net::{Ipv4Addr, TcpListener};

use orbit_core::OrbitError;

use super::super::DEFAULT_DASHBOARD_PORT;
use super::super::connect::{
    ConnectArgs, reject_root_override, remote_serve_command, select_local_port, tunnel_spec,
};

/// Minimal args builder so each test states only what it cares about.
fn args(host: &str, remote_port: u16, workspace: Option<&str>) -> ConnectArgs {
    ConnectArgs {
        ssh_host: host.to_string(),
        port: None,
        remote_port,
        workspace: workspace.map(str::to_string),
        global: false,
        no_open: false,
    }
}

// ── port selection ────────────────────────────────────────────────────────

#[test]
fn explicit_busy_port_errors() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
    let busy = listener.local_addr().expect("addr").port();
    let err = select_local_port(Some(busy)).expect_err("busy port must error");
    assert!(matches!(err, OrbitError::InvalidInput(_)));
}

#[test]
fn auto_selection_falls_back_when_default_is_busy() {
    // Occupy the preferred default port; auto-selection must pick something
    // else. If 7878 is already taken by another process in this environment,
    // the same fallback path runs but we cannot assert deterministically, so
    // we skip in that case.
    if let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, DEFAULT_DASHBOARD_PORT)) {
        let chosen = select_local_port(None).expect("auto select");
        assert_ne!(chosen, DEFAULT_DASHBOARD_PORT);
        drop(listener);
    }
}

// ── root override ─────────────────────────────────────────────────────────

#[test]
fn root_override_is_rejected_with_the_workspace_flag_in_the_message() {
    // ORB-11388: `--root` no longer names the remote workspace, and this
    // command has no local data directory to override, so it must fail loudly
    // and point at `--workspace` instead of dropping the preselection.
    assert!(reject_root_override(None).is_ok());
    let err = reject_root_override(Some(std::path::Path::new("/srv/ws")))
        .expect_err("--root must be rejected");
    let OrbitError::InvalidInput(message) = &err else {
        panic!("expected invalid input, got {err:?}");
    };
    assert!(message.contains("--workspace"), "{message}");
}

// ── remote command construction ───────────────────────────────────────────

#[test]
fn remote_command_always_passes_no_open() {
    // The remote must never open a browser on the box regardless of the local
    // `--no-open` flag (which only controls *our* browser).
    let mut cfg = args("box", 7878, None);
    cfg.no_open = true;
    assert!(remote_serve_command(&cfg).contains("--no-open"));
    cfg.no_open = false;
    assert!(remote_serve_command(&cfg).contains("--no-open"));
}

#[test]
fn remote_command_shell_quotes_workspace() {
    let cmd = remote_serve_command(&args("box", 7878, Some("/srv/my ws")));
    assert!(
        cmd.contains("--workspace '/srv/my ws'"),
        "a workspace selector with a space must be single-quoted: {cmd}"
    );
}

#[test]
fn remote_command_never_forwards_root() {
    // ORB-11388: `--root` selects the remote's *registry* now. Forwarding a
    // workspace path there would serve an empty registry instead of
    // preselecting that workspace.
    let cmd = remote_serve_command(&args("box", 7878, Some("/srv/ws")));
    assert!(!cmd.contains("--root"), "{cmd}");
}

#[test]
fn remote_command_without_workspace_has_no_workspace_flag() {
    let cmd = remote_serve_command(&args("box", 7878, None));
    assert!(!cmd.contains("--workspace"));
}

#[test]
fn remote_command_passes_global_when_set() {
    let mut cfg = args("box", 7878, None);
    assert!(!remote_serve_command(&cfg).contains("--global"));
    cfg.global = true;
    let cmd = remote_serve_command(&cfg);
    assert!(
        cmd.contains("--global"),
        "remote serve must receive --global: {cmd}"
    );
}

#[test]
fn remote_command_combines_global_and_workspace() {
    let mut cfg = args("box", 7878, Some("/srv/ws"));
    cfg.global = true;
    let cmd = remote_serve_command(&cfg);
    assert!(cmd.contains("--global"), "{cmd}");
    assert!(cmd.contains("--workspace '/srv/ws'"), "{cmd}");
}

// ── tunnel wiring ─────────────────────────────────────────────────────────

#[test]
fn tunnel_spec_forwards_the_requested_ports_and_remote_command() {
    let spec = tunnel_spec(&args("user@host", 9000, None), 7000);
    assert_eq!(spec.ssh_host, "user@host");
    assert_eq!(spec.local_port, 7000);
    assert_eq!(spec.remote_port, 9000);
    assert_eq!(spec.remote_command, "orbit web serve --no-open --port 9000");
    assert_eq!(spec.remote_description, "orbit web serve");
    assert!(
        spec.readiness_target.contains("localhost:7000/healthz"),
        "the readiness target names the forwarded local port: {}",
        spec.readiness_target
    );
}

#[test]
fn tunnel_spec_waits_longer_for_a_spawned_server_than_for_an_attach_probe() {
    // Attaching only has to cover an SSH handshake; spawning has to cover a
    // remote process boot. Collapsing the two would either make attach slow or
    // make spawn flaky.
    let spec = tunnel_spec(&args("box", 7878, None), 7878);
    assert!(spec.ready_timeout > spec.attach_timeout);
}
