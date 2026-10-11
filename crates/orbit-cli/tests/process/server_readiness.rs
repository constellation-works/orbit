//! Deterministic negative controls for owned-server readiness.

use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::bound_server::{Announcement, BoundServer};
use orbit_common::test_env;

fn orbit(work: &Path, home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("RUST_LOG", "warn,orbit::command::mcp::server=info")
        .env("NO_COLOR", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

#[test]
fn a_decoy_listener_cannot_make_a_failed_bind_ready() {
    let temp = tempfile::tempdir().expect("fixture directory");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("fixture home");
    let initialized = orbit(temp.path(), &home)
        .args([
            "init",
            "--non-interactive",
            "--machine-name",
            "readiness",
            "--task-prefix",
            "RDY",
        ])
        .output()
        .expect("initialize fixture");
    assert!(initialized.status.success(), "{initialized:?}");

    // Keep this socket owned throughout both real server launches. There is
    // no port churn and neither child can bind the decoy's address.
    let decoy = TcpListener::bind("127.0.0.1:0").expect("owned decoy");
    let address = decoy.local_addr().expect("decoy address");
    for (kind, args) in [
        (
            Announcement::Dashboard,
            vec![
                "web".into(),
                "serve".into(),
                "--port".into(),
                address.port().to_string(),
                "--no-open".into(),
            ],
        ),
        (
            Announcement::Mcp,
            vec!["mcp".into(), "listen".into(), address.to_string()],
        ),
    ] {
        let mut server = BoundServer::new(
            orbit(temp.path(), &home)
                .args(args)
                .spawn()
                .expect("launch intended server"),
            kind,
        );
        let error = server
            .wait_for_address(test_env::FIXTURE_STEP_DEADLINE)
            .expect_err("an unrelated listener must never establish child readiness");
        let diagnostic = error.to_string();
        assert!(
            diagnostic.contains("Address already in use")
                && diagnostic.contains(&address.to_string()),
            "must observe the intended child's bind failure: {diagnostic}"
        );
        assert!(
            !server
                .try_wait()
                .expect("poll failed server")
                .expect("server exited")
                .success()
        );
        assert!(
            TcpStream::connect_timeout(&address, Duration::from_secs(5)).is_ok(),
            "decoy remains connectable after the intended child exits"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_reaped_childs_queued_announcement_cannot_make_a_decoy_ready() {
    let decoy = TcpListener::bind("127.0.0.1:0").expect("owned decoy");
    let address = decoy.local_addr().expect("decoy address");
    for (kind, line, script) in [
        (
            Announcement::Dashboard,
            format!("Dashboard listening on http://{address}"),
            "printf '%s\\n' \"$1\"; exit 41",
        ),
        (
            Announcement::Mcp,
            format!("INFO orbit::command::mcp::server: orbit mcp listener bound address={address}"),
            "printf '%s\\n' \"$1\" >&2; exit 41",
        ),
    ] {
        let mut server = BoundServer::new(
            Command::new("/bin/sh")
                .args(["-c", script, "readiness-fixture", &line])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("launch announced-but-exited child"),
            kind,
        );
        assert_eq!(
            server.wait().expect("deterministically reap child").code(),
            Some(41)
        );
        let error = server
            .wait_for_address(Duration::from_secs(5))
            .expect_err("queued announcements from a reaped child must be rejected");
        assert!(
            error.to_string().contains("server exited before readiness"),
            "{error}"
        );
        assert!(
            TcpStream::connect_timeout(&address, Duration::from_secs(5)).is_ok(),
            "decoy stays connectable throughout the negative control"
        );
    }
}
