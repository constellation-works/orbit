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

// SIGTERM delivery is the behaviour under test.
#![cfg(unix)]
#![allow(missing_docs)]
// Integration fixtures use expect/unwrap for concise failure diagnostics.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};

use crate::bound_server::{Announcement, BoundServer};
use crate::web_fixture_io::{DeadlineStream, OPERATION_TIMEOUT, http_get, http_get_with_timeout};
use std::time::{Duration, Instant};

use orbit_common::test_env;
use tempfile::tempdir;

/// Upper bound this test allows between SIGTERM and process exit. Generous
/// relative to orbit-web's own `SHUTDOWN_GRACE_PERIOD` (10s, see
/// `crates/orbit-web/src/serve.rs`) so the assertion only fires on a real
/// regression back to "hangs until systemd's SIGKILL", not on CI jitter, while
/// staying far below `orbit-web.service`'s `TimeoutStopUSec=90s`.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(20);

/// Bound how much of a peer's response we accept before finding the HTTP head
/// delimiter. `DeadlineStream` independently bounds the elapsed time.
const SSE_RESPONSE_HEAD_LIMIT: usize = 16 * 1024;

/// How long to wait for the dashboard to start accepting connections.
const STARTUP_DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn dashboard_exits_promptly_on_sigterm_with_open_connections() {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("create home");

    let mut server = spawn_dashboard(&home);
    let port = server
        .wait_for_address(STARTUP_DEADLINE)
        .expect("dashboard bound")
        .port();

    // Idle connection: accepted by the listener, no request in flight. Must
    // not, by itself, block a graceful shutdown.
    let idle = DeadlineStream::connect(port, OPERATION_TIMEOUT).expect("open idle connection");

    // Active connection: an in-flight `/api/log/stream` SSE response, the
    // exact shape of open connection the 2026-09-05 restart hung on. Confirm
    // it is actually streaming before sending SIGTERM.
    let active = open_sse(port, OPERATION_TIMEOUT);

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
         within {SHUTDOWN_DEADLINE:?}, got {status:?} after {shutdown_duration:?}; \
         stderr tail: {}",
        server.stderr_tail()
    );

    drop(idle);
    drop(active);

    // A fresh server on a new port must come up and serve /healthz normally:
    // shutdown must not have wedged process-owned resources (log-stream gate
    // permits, etc) that a restarted process would inherit.
    let mut restarted = spawn_dashboard(&home);
    let port2 = restarted
        .wait_for_address(STARTUP_DEADLINE)
        .expect("restarted dashboard bound")
        .port();
    let body = http_get(port2, "/healthz");
    assert!(
        body.contains("ok"),
        "expected /healthz to report ok after restart, got: {body}"
    );

    let before_sigterm2 = Instant::now();
    send_sigterm(&restarted);
    let status2 = wait_with_deadline(&mut restarted, SHUTDOWN_DEADLINE)
        .expect("restarted server did not exit within the shutdown deadline");
    let shutdown_duration2 = before_sigterm2.elapsed();
    assert!(
        status2.success(),
        "expected the restarted dashboard to exit cleanly within {SHUTDOWN_DEADLINE:?}, \
         got {status2:?} after {shutdown_duration2:?}; stderr tail: {}",
        restarted.stderr_tail()
    );
}

fn open_sse(port: u16, timeout: Duration) -> DeadlineStream {
    let mut active = DeadlineStream::connect(port, timeout).expect("open active connection");
    active
        .write_all(
            b"GET /api/log/stream HTTP/1.1\r\n\
              Host: 127.0.0.1\r\n\
              Connection: keep-alive\r\n\r\n",
        )
        .expect("send SSE request");
    let head = read_http_head(&mut active, SSE_RESPONSE_HEAD_LIMIT)
        .expect("read complete SSE response head");
    assert_sse_response_head(&head).unwrap_or_else(|error| {
        panic!(
            "expected an open SSE stream before sending SIGTERM: {error}; head={:?}",
            String::from_utf8_lossy(&head)
        )
    });

    active
}

fn read_http_head(reader: &mut impl Read, byte_limit: usize) -> io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(byte_limit.min(1024));
    loop {
        if let Some(delimiter) = head.windows(4).position(|window| window == b"\r\n\r\n") {
            head.truncate(delimiter + 4);
            return Ok(head);
        }
        if head.len() == byte_limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("HTTP response head exceeded {byte_limit} bytes without a delimiter"),
            ));
        }

        let remaining = byte_limit - head.len();
        let mut buffer = [0_u8; 1024];
        let read_limit = remaining.min(buffer.len());
        let count = reader.read(&mut buffer[..read_limit])?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "HTTP response ended before the complete head delimiter",
            ));
        }
        head.extend_from_slice(&buffer[..count]);
    }
}

fn assert_sse_response_head(head: &[u8]) -> io::Result<()> {
    if !head.ends_with(b"\r\n\r\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP response head is not terminated",
        ));
    }
    let text = std::str::from_utf8(head).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("HTTP head is not UTF-8: {error}"),
        )
    })?;
    let mut lines = text[..text.len() - 4].split("\r\n");
    let status_line = lines.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP response head has no status line",
        )
    })?;
    let mut status_fields = status_line.split_whitespace();
    let version = status_fields.next();
    let status = status_fields.next();
    if !matches!(version, Some("HTTP/1.0" | "HTTP/1.1")) || status != Some("200") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected HTTP/1.x status 200, got {status_line:?}"),
        ));
    }

    let mut content_type = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("malformed HTTP response header {line:?}"),
            )
        })?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("malformed HTTP response header name {name:?}"),
            ));
        }
        if name.eq_ignore_ascii_case("content-type") && content_type.replace(value.trim()).is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP response has multiple Content-Type headers",
            ));
        }
    }

    let content_type = content_type.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP response has no Content-Type header",
        )
    })?;
    if !content_type
        .split(';')
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("text/event-stream"))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected text/event-stream, got {content_type:?}"),
        ));
    }
    Ok(())
}

/// A stalled peer must fail before releasing the peer, unwind through the
/// live dashboard's guard, and reap it instead of leaking a fixture process.
#[test]
fn stalled_http_and_sse_reads_fail_with_deadlines_and_reap_the_dashboard() {
    for (name, sse, prefix) in [
        ("HTTP without response bytes", false, b"".as_slice()),
        (
            "HTTP without EOF",
            false,
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nok".as_slice(),
        ),
        ("SSE without response head", true, b"".as_slice()),
        (
            "SSE response head without its delimiter",
            true,
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n".as_slice(),
        ),
    ] {
        exercise_stalled_peer(name, sse, prefix);
    }
}

fn exercise_stalled_peer(name: &str, sse: bool, prefix: &'static [u8]) {
    let temp = tempdir().expect("tempdir");
    let home = temp.path().join("home");
    std::fs::create_dir_all(&home).expect("create home");
    let mut port = None;
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind stalled peer");
    let peer_port = listener.local_addr().expect("stalled peer address").port();
    listener.set_nonblocking(true).expect("nonblocking accept");
    let (release, released) = std::sync::mpsc::sync_channel(1);
    let (request_seen, seen) = std::sync::mpsc::sync_channel(1);
    let mut spawned = None;
    let mut started = None;
    let (result, elapsed) = std::thread::scope(|scope| {
        let peer = scope.spawn(move || {
            // Safety ceilings also bound a broken client/fixture. Successful
            // execution releases this peer explicitly, never by sleeping.
            let deadline = Instant::now() + test_env::FIXTURE_STEP_DEADLINE;
            let mut socket = loop {
                if released.try_recv().is_ok() {
                    return;
                }
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "stalled peer accept deadline");
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(error) => panic!("stalled peer accept: {error}"),
                }
            };
            socket.set_nonblocking(false).expect("blocking peer IO");
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .expect("stalled peer request deadline");
                socket
                    .set_read_timeout(Some(remaining))
                    .expect("peer read timeout");
                let mut byte = [0];
                socket.read_exact(&mut byte).expect("peer reads request");
                request.push(byte[0]);
                assert!(request.len() < 4096, "fixture request is bounded");
            }
            socket
                .set_write_timeout(Some(test_env::FIXTURE_STEP_DEADLINE))
                .expect("peer write timeout");
            socket.write_all(prefix).expect("write partial response");
            request_seen.send(()).expect("report request received");
            released
                .recv_timeout(test_env::FIXTURE_STEP_DEADLINE)
                .expect("peer must be released after the client fails");
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut server = spawn_dashboard(&home);
            port = Some(
                server
                    .wait_for_address(STARTUP_DEADLINE)
                    .expect("dashboard bound")
                    .port(),
            );
            assert!(matches!(server.try_wait(), Ok(None)), "dashboard is alive");
            spawned = Some(server.id());
            started = Some(Instant::now());
            let timeout = Duration::from_secs(1);
            if sse {
                let _ = open_sse(peer_port, timeout);
            } else {
                http_get_with_timeout(peer_port, "/healthz", timeout);
            }
        }));
        let elapsed = started.map(|started| started.elapsed());
        let _ = release.send(());
        peer.join().expect("stalled peer completed");
        (result, elapsed)
    });
    seen.try_recv()
        .expect("peer received the full fixture request");
    let pid = spawned.expect("dashboard was listening before the IO failure");
    let failure = result.expect_err("stalled IO must unwind through the child guard");
    let diagnostic = failure
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| failure.downcast_ref::<&str>().copied())
        .expect("IO panic includes a diagnostic");
    assert!(
        diagnostic.contains("response read")
            && diagnostic.contains("fixture operation deadline")
            && diagnostic.contains(&peer_port.to_string()),
        "{name} must report the failed operation, deadline and peer: {diagnostic}"
    );
    assert!(
        elapsed.expect("IO started") < Duration::from_secs(10),
        "{name} must bound the 1s IO deadline plus child cleanup, got {elapsed:?}"
    );
    // SAFETY: signal zero only probes the fixture PID; waitpid verifies that
    // the guard reaped it rather than leaving a zombie behind.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert_eq!(
        unsafe { libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    assert!(
        DeadlineStream::connect(port.expect("dashboard bound"), OPERATION_TIMEOUT).is_err(),
        "the dashboard must release its listening port during unwinding"
    );
}

/// The scripted dashboard QA recipe in `docs/DEVELOPMENT.md` must not abandon
/// its server when SIGTERM lands between the spawn, the PID capture and the
/// token capture (ORB-25593), nor terminate a background job its caller
/// started when it runs in the documented `( )` subshell and the signal lands
/// before the spawn (ORB-25606: the subshell inherits the caller's `$!`). A
/// Bash `DEBUG` hook fires the signal deterministically just before the spawn
/// and each assignment; a Linux subreaper parent then proves the exact stub
/// child was reaped by the harness, not merely killed and left as a zombie
/// for the parent to collect.
#[cfg(target_os = "linux")]
#[test]
fn dashboard_qa_recipe_reaps_child_when_interrupted_at_identity_boundaries() {
    for boundary in ["before_spawn", "spawn_pid", "pid_token", "none"] {
        exercise_qa_recipe_boundary(boundary);
    }
}

#[cfg(target_os = "linux")]
fn exercise_qa_recipe_boundary(boundary: &str) {
    use std::path::Path;

    let temp = tempdir().expect("tempdir");
    let qa_base = temp.path().join("qa");
    let shots = qa_base.join("shots");
    std::fs::create_dir_all(&shots).expect("create shots");
    std::fs::write(shots.join("sentinel.txt"), "keep").expect("write sentinel");
    let qa_repo = temp.path().join("repo");
    std::fs::create_dir_all(&qa_repo).expect("create qa repo");

    let docs = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/DEVELOPMENT.md"),
    )
    .expect("read DEVELOPMENT.md");
    let section = docs
        .find("#### Scripted dashboard QA")
        .expect("scripted QA section");
    let fence = docs[section..]
        .find("```bash\n")
        .map(|index| section + index + "```bash\n".len())
        .expect("scripted QA bash fence");
    let end = docs[fence..]
        .find("\n```")
        .map(|index| fence + index)
        .expect("scripted QA fence end");
    let mut recipe = docs[fence..end].to_string();

    let serve_start = recipe.find("SERVE_CMD=(").expect("SERVE_CMD line");
    let serve_end = recipe[serve_start..]
        .find('\n')
        .map(|index| serve_start + index)
        .expect("SERVE_CMD line end");
    recipe.replace_range(
        serve_start..serve_end,
        r#"SERVE_CMD=(/bin/sh -c 'echo $$ >"$QA_BASE/child.pid"; echo "Dashboard listening on http://127.0.0.1:9"; exec sleep 300')"#,
    );
    let hook = r#"
browser_start() { :; }
browser_shoot() { echo shot > "$SHOTS/dashboard-dark-1280.png"; }
browser_close() { :; }
__boundary_fired=
__hook() {
  case "$BASH_COMMAND" in
    'qa_serve_start 30') [ "$QA_BOUNDARY" = before_spawn ] || return 0 ;;
    'SERVER_PID=$!') [ "$QA_BOUNDARY" = spawn_pid ] || return 0 ;;
    'SERVER_TOKEN="$(qa_token "$SERVER_PID")"') [ "$QA_BOUNDARY" = pid_token ] || return 0 ;;
    *) return 0 ;;
  esac
  [ -n "$__boundary_fired" ] && return 0
  __boundary_fired=1
  trap - DEBUG
  kill -TERM "$BASHPID"
}
trap __hook DEBUG
set -T
"#;
    let capture = recipe
        .find("# Capture.")
        .expect("capture marker in QA recipe");
    recipe.insert_str(capture, hook);
    // The documented form: the recipe runs in a `( )` subshell, after the
    // caller started a background job whose PID the subshell inherits in `$!`.
    let recipe = format!(
        r#"sleep 300 &
caller_job=$!
(
{recipe}
)
sub_rc=$?
sleep 0.3
if kill -0 "$caller_job" 2>/dev/null; then echo caller_job=alive; kill -KILL "$caller_job"; else echo caller_job=terminated; fi
exit "$sub_rc"
"#
    );
    let script_path = temp.path().join("script.sh");
    std::fs::write(&script_path, recipe).expect("write QA harness");

    const RUNNER: &str = r#"
import ctypes, os, subprocess, sys, time
script, child_pid_file = sys.argv[1], sys.argv[2]
libc = ctypes.CDLL(None)
if libc.prctl(36, 1, 0, 0, 0) != 0:
    raise SystemExit("prctl(PR_SET_CHILD_SUBREAPER) failed")
start = time.monotonic()
proc = subprocess.Popen(["bash", script])
timed_out = 0
try:
    rc = proc.wait(timeout=10)
except subprocess.TimeoutExpired:
    timed_out = 1
    proc.kill()
    rc = proc.wait(timeout=5)
print(f"harness_timeout={timed_out}")
print(f"harness_rc={rc}")
print(f"elapsed_ms={int((time.monotonic() - start) * 1000)}")
child_state = "missing"
child_pid = ""
deadline = time.monotonic() + 2
while time.monotonic() < deadline:
    try:
        child_pid = open(child_pid_file).read().strip()
        child = int(child_pid)
    except (OSError, ValueError):
        time.sleep(0.05)
        continue
    try:
        wpid, _status = os.waitpid(child, os.WNOHANG)
    except ChildProcessError:
        child_state = "reaped"
        break
    if wpid == child:
        child_state = "zombie_left_by_harness"
        break
    child_state = "alive"
    time.sleep(0.05)
else:
    if child_pid:
        child_state = "alive"
if child_state == "alive":
    try:
        os.kill(int(child_pid), 9)
        os.waitpid(int(child_pid), 0)
    except OSError:
        pass
print(f"child_pid={child_pid}")
print(f"child_state={child_state}")
"#;
    let child_pid_file = qa_base.join("child.pid");
    let mut runner = Command::new("/usr/bin/python3")
        .arg("-c")
        .arg(RUNNER)
        .arg(&script_path)
        .arg(&child_pid_file)
        .env("QA_BASE", &qa_base)
        .env("QA_HOME", temp.path().join("home"))
        .env("QA_REPO", &qa_repo)
        .env("QA_BOUNDARY", boundary)
        .env("QA_TERM_GRACE", "5")
        .env("ORBIT_BIN", "/bin/true")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn subreaper runner");
    let started = Instant::now();
    let status = wait_with_deadline(&mut runner, Duration::from_secs(25))
        .unwrap_or_else(|| panic!("{boundary}: subreaper runner did not finish"));
    let output = runner.wait_with_output().expect("collect runner output");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let field = |name: &str| {
        stdout
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap_or("")
            .to_string()
    };
    assert!(
        status.success(),
        "{boundary}: runner failed: {stdout} {stderr}"
    );
    assert_eq!(
        field("harness_timeout="),
        "0",
        "{boundary}: harness exceeded its shutdown bound: {stdout} {stderr}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{boundary}: harness took too long: {stdout}"
    );
    assert_eq!(
        field("caller_job="),
        "alive",
        "{boundary}: the caller's own background job must be left untouched: {stdout} {stderr}"
    );
    let expected_rc = if boundary == "none" { "0" } else { "143" };
    assert_eq!(
        field("harness_rc="),
        expected_rc,
        "{boundary}: unexpected harness exit: {stdout} {stderr}"
    );
    // Before the spawn there is no server: nothing may have been started.
    let expected_child = if boundary == "before_spawn" {
        "missing"
    } else {
        "reaped"
    };
    assert_eq!(
        field("child_state="),
        expected_child,
        "{boundary}: exact child must be reaped by the harness: {stdout} {stderr}"
    );
    // The log is created by the start itself, so it exists only after the spawn.
    assert!(
        shots.join("sentinel.txt").exists()
            && (boundary == "before_spawn" || qa_base.join("serve.log").exists()),
        "{boundary}: teardown must leave artifacts in place: {stdout}"
    );
    if boundary == "none" {
        assert!(
            shots.join("dashboard-dark-1280.png").exists(),
            "normal recipe run must keep its capture: {stdout}"
        );
    }
}

fn spawn_dashboard(home: &std::path::Path) -> BoundServer {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit"));
    // ORB-11300: the dashboard serves whichever workspace it resolves. Without
    // this the inherited `ORBIT_REGISTRY_ROOT`/`ORBIT_WORKSPACE` pair pointed
    // it at the live one instead of this fixture's empty home.
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    BoundServer::spawn(
        command
            .env("HOME", home)
            .env("USERPROFILE", home)
            .args(["web", "serve", "--port", "0", "--no-open"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
        Announcement::Dashboard,
    )
    .expect("spawn orbit web serve")
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
