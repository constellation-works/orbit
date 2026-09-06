//! `orbit web serve` must exit promptly on SIGTERM even with open dashboard
//! connections (ORB-11246).
//!
//! Incident: a live `systemctl --user restart orbit-web.service` sent SIGTERM
//! to the running dashboard, but the process stayed in `stop-sigterm` for the
//! full `TimeoutStopUSec=90s` before systemd fell back to SIGKILL. Root cause:
//! `axum::serve(...).with_graceful_shutdown(...)` waits indefinitely for every
//! open connection to finish, and `/api/log/stream` (crates/orbit-web/src/api/log.rs)
//! is a long-lived SSE stream that only ends when the client disconnects or
//! the server tells it to close — a browser dashboard tab left open across a
//! restart never does either on its own.
//!
//! This test spawns the real `orbit web serve` binary, opens one idle and one
//! active (in-flight SSE) connection against it, sends SIGTERM, and asserts
//! the process exits well before systemd would have force-killed it — without
//! this test's own cleanup path (`Child::kill`, which is SIGKILL) ever firing.
//! It then confirms a fresh server on a new port serves `/healthz` normally,
//! proving the fix does not leave shutdown half-done.

#![allow(missing_docs)]
// Integration fixtures use expect/unwrap for concise failure diagnostics.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use orbit_common::test_env;
use tempfile::tempdir;

/// Upper bound this test allows between SIGTERM and process exit. Generous
/// relative to orbit-web's own `SHUTDOWN_GRACE_PERIOD` (10s, see
/// `crates/orbit-web/src/lib.rs`) so the assertion only fires on a real
/// regression back to "hangs until systemd's SIGKILL", not on CI jitter, while
/// staying far below `orbit-web.service`'s `TimeoutStopUSec=90s`.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(20);

/// How long to wait for the dashboard to start accepting connections.
const STARTUP_DEADLINE: Duration = Duration::from_secs(10);

/// Must exceed orbit-web's own `SHUTDOWN_GRACE_PERIOD` (10s, see
/// `crates/orbit-web/src/lib.rs`) so that outliving it is meaningful evidence:
/// ORB-11255's regression started that grace-period timeout counting down
/// when serving began rather than when a shutdown signal arrived, so an
/// unsignaled server was killed at exactly this deadline.
const LONGER_THAN_SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(13);

/// ORB-11255: a server that never received a shutdown signal must keep
/// serving indefinitely -- in particular, past orbit-web's own
/// `SHUTDOWN_GRACE_PERIOD`. PR1328 (ORB-11246) introduced a regression where
/// `tokio::time::timeout(SHUTDOWN_GRACE_PERIOD, drain)` started counting down
/// as soon as serving began, so a perfectly healthy, never-signaled server
/// exited (successfully!) once that timeout elapsed. This test fails against
/// that placement: it waits longer than the grace period with no signal sent
/// at all, then confirms the server is still alive and serving `/healthz`,
/// before confirming SIGTERM still produces a prompt, clean shutdown.
#[test]
fn dashboard_outlives_shutdown_grace_period_when_no_signal_is_sent() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("create home");

    let port = free_port();
    let mut server = spawn_dashboard(&home, port);
    wait_for_listening(port);

    std::thread::sleep(LONGER_THAN_SHUTDOWN_GRACE_PERIOD);

    assert!(
        matches!(server.try_wait(), Ok(None)),
        "orbit web serve exited on its own after {LONGER_THAN_SHUTDOWN_GRACE_PERIOD:?} \
         with no shutdown signal ever sent -- this is the ORB-11255 regression: \
         the shutdown grace-period timeout must not start until a signal arrives"
    );

    let body = http_get(port, "/healthz");
    assert!(
        body.contains("ok"),
        "expected /healthz to still report ok after outliving the shutdown \
         grace period with no signal sent, got: {body}"
    );

    let before_sigterm = Instant::now();
    send_sigterm(&server);
    let status = wait_with_deadline(&mut server, SHUTDOWN_DEADLINE).unwrap_or_else(|| {
        panic!(
            "orbit web serve did not exit within {SHUTDOWN_DEADLINE:?} of SIGTERM \
             after having outlived the shutdown grace period once already"
        )
    });
    assert!(
        status.success(),
        "expected a clean shutdown within {SHUTDOWN_DEADLINE:?}, got {status:?} after {:?}",
        before_sigterm.elapsed()
    );
}

#[test]
fn dashboard_exits_promptly_on_sigterm_with_open_connections() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("create home");

    let port = free_port();
    let mut server = spawn_dashboard(&home, port);
    wait_for_listening(port);

    // Idle connection: accepted by the listener, no request in flight. Must
    // not, by itself, block a graceful shutdown.
    let idle = TcpStream::connect(("127.0.0.1", port)).expect("open idle connection");

    // Active connection: an in-flight `/api/log/stream` SSE response, the
    // exact shape of open connection the 2026-09-05 restart hung on. Confirm
    // it is actually streaming before sending SIGTERM.
    let mut active = TcpStream::connect(("127.0.0.1", port)).expect("open active connection");
    active
        .write_all(
            b"GET /api/log/stream HTTP/1.1\r\n\
              Host: 127.0.0.1\r\n\
              Connection: keep-alive\r\n\r\n",
        )
        .expect("send SSE request");
    let mut head = [0_u8; 4096];
    let n = active.read(&mut head).expect("read SSE response head");
    let head = String::from_utf8_lossy(&head[..n]);
    assert!(
        head.contains("text/event-stream"),
        "expected an open SSE stream before sending SIGTERM, got: {head}"
    );

    let before_sigterm = Instant::now();
    send_sigterm(&server);

    let status = wait_with_deadline(&mut server, SHUTDOWN_DEADLINE).unwrap_or_else(|| {
        panic!(
            "orbit web serve did not exit within {SHUTDOWN_DEADLINE:?} of SIGTERM \
             with an open SSE connection -- this is the systemd stop-sigterm \
             timeout this test guards against (ORB-11246)"
        )
    });
    let shutdown_duration = before_sigterm.elapsed();
    // `shutdown_duration` is the restart-duration evidence for the
    // acceptance criterion; the assertion message below reports it.
    assert!(
        status.success(),
        "expected the in-process graceful-shutdown path to exit cleanly \
         within {SHUTDOWN_DEADLINE:?}, got {status:?} after {shutdown_duration:?}"
    );

    drop(idle);
    drop(active);

    // A fresh server on a new port must come up and serve /healthz normally:
    // shutdown must not have wedged process-owned resources (log-stream gate
    // permits, etc) that a restarted process would inherit.
    let port2 = free_port();
    let mut restarted = spawn_dashboard(&home, port2);
    wait_for_listening(port2);
    let body = http_get(port2, "/healthz");
    assert!(
        body.contains("ok"),
        "expected /healthz to report ok after restart, got: {body}"
    );

    send_sigterm(&restarted);
    let status2 = wait_with_deadline(&mut restarted, SHUTDOWN_DEADLINE)
        .expect("restarted server did not exit within the shutdown deadline");
    assert!(status2.success());
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

fn spawn_dashboard(home: &std::path::Path, port: u16) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit"));
    // ORB-11300: the dashboard serves whichever workspace it resolves. Without
    // this the inherited `ORBIT_REGISTRY_ROOT`/`ORBIT_WORKSPACE` pair pointed
    // it at the live one instead of this fixture's empty home.
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .args(["web", "serve", "--port", &port.to_string(), "--no-open"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn orbit web serve")
}

fn wait_for_listening(port: u16) {
    let deadline = Instant::now() + STARTUP_DEADLINE;
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "orbit web serve did not start listening on port {port} within {STARTUP_DEADLINE:?}"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn send_sigterm(child: &Child) {
    // Safety: `kill` targets this test's own already-spawned child pid with
    // SIGTERM -- the same signal systemd sends on `systemctl restart`
    // (ORB-11246) -- and performs no other side effect.
    let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(
        rc,
        0,
        "failed to send SIGTERM: {}",
        std::io::Error::last_os_error()
    );
}

/// Poll for exit up to `deadline`. On timeout, force-kill (SIGKILL) so a
/// failing test doesn't leak a listening process into the rest of the CI
/// box -- that cleanup is not part of the behavior under test.
fn wait_with_deadline(child: &mut Child, deadline: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Some(status);
        }
        if start.elapsed() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn http_get(port: u16, path: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    response
}
