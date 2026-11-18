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
/// and each assignment. The two post-spawn boundaries hold the stub on a FIFO
/// before it can publish its PID; the hook records `$!`, and a Linux
/// subreaper parent proves that exact child was reaped. `unreaped` and
/// `timeout` leave that child and a SIGTERM-ignoring descendant alive, and
/// the parent must notice and sweep the process group it owns.
#[cfg(target_os = "linux")]
#[test]
fn dashboard_qa_recipe_reaps_child_when_interrupted_at_identity_boundaries() {
    for boundary in [
        "before_spawn",
        "spawn_pid",
        "pid_token",
        "none",
        "unreaped",
        "timeout",
    ] {
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
        // Hold `spawn_pid` and `pid_token` before `$$` is published, so the
        // runner cannot learn the PID from the child. `unreaped` and `timeout`
        // also spawn a descendant that ignores SIGTERM, then block the same way.
        r#"SERVE_CMD=(/bin/sh -c 'case "$QA_BOUNDARY" in unreaped|timeout) (trap "" TERM; exec sleep 300) & printf "%s\n" "$!" >"$QA_BASE/descendant.pid"; printf ready >"$QA_BASE/ready.fifo"; read -r _ <"$QA_BASE/hold.fifo" ;; spawn_pid|pid_token) read -r _ <"$QA_BASE/hold.fifo" ;; esac; echo $$ >"$QA_BASE/child.pid"; echo "Dashboard listening on http://127.0.0.1:9"; exec sleep 300')"#,
    );
    // Patterns are single-quoted: `$!` inside double quotes would expand in
    // the DEBUG trap and miss the assignment command.
    let hook = r#"
browser_start() { :; }
browser_shoot() { echo shot > "$SHOTS/dashboard-dark-1280.png"; }
browser_close() { :; }
__boundary_fired=
__record_child() {
  [ -n "${!:-}" ] || return 0
  [ "${!:-}" = "${QA_SPAWN_PRIOR:-}" ] && return 0
  printf '%s\n' "$!" > "$QA_BASE/observed.pid"
}
__hook() {
  case "$BASH_COMMAND" in
    'qa_serve_start 30') [ "$QA_BOUNDARY" = before_spawn ] || return 0 ;;
    'SERVER_PID=$!')
      if [ "$QA_BOUNDARY" = unreaped ] || [ "$QA_BOUNDARY" = timeout ]; then
        read -r _ < "$QA_BASE/ready.fifo"
      fi
      __record_child
      case "$QA_BOUNDARY" in spawn_pid|unreaped) ;; *) return 0 ;; esac
      ;;
    'SERVER_TOKEN="$(qa_token "$SERVER_PID")"')
      __record_child
      [ "$QA_BOUNDARY" = pid_token ] || return 0
      ;;
    *) return 0 ;;
  esac
  [ -n "$__boundary_fired" ] && return 0
  __boundary_fired=1
  trap - DEBUG
  if [ "$QA_BOUNDARY" = unreaped ]; then kill -KILL "$BASHPID"; else kill -TERM "$BASHPID"; fi
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
echo "caller_job_pid=$caller_job"
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
import ctypes, os, signal, subprocess, sys, time
script, child_pid_file, observed_pid_file, descendant_pid_file = sys.argv[1:5]
libc = ctypes.CDLL(None)
if libc.prctl(36, 1, 0, 0, 0) != 0:
    raise SystemExit("prctl(PR_SET_CHILD_SUBREAPER) failed")

def proc_state(pid):
    try:
        with open(f"/proc/{pid}/stat") as handle:
            stat = handle.read()
    except OSError:
        return None
    try:
        return stat.rsplit(")", 1)[1].split()[0]
    except (IndexError, ValueError):
        return None

def read_pid(path):
    try:
        pid = int(open(path).read().strip())
    except (OSError, ValueError):
        return 0
    return pid if pid > 1 else 0

def classify(pid):
    if not pid:
        return "missing"
    state = proc_state(pid)
    if state == "Z":
        return "zombie_left_by_harness"
    if state is not None:
        return "alive"
    try:
        waited, _status = os.waitpid(pid, os.WNOHANG)
    except ChildProcessError:
        return "reaped"
    if waited == pid:
        return "zombie_left_by_harness"
    return "reaped"

def group_members(pgid):
    try:
        names = os.listdir("/proc")
    except OSError:
        return None
    members = []
    for name in names:
        if not name.isdigit():
            continue
        pid = int(name)
        if pid <= 1:
            continue
        try:
            if os.getpgid(pid) == pgid:
                members.append(pid)
        except OSError:
            continue
    return members

def others_alive():
    members = group_members(os.getpgrp())
    if members is None:
        return True
    me = os.getpid()
    return any(pid != me for pid in members)

def reap_zombies():
    while True:
        try:
            pid, _status = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            return
        if pid == 0:
            return

def signal_others(sig):
    members = group_members(os.getpgrp())
    if members is None:
        return False
    me = os.getpid()
    for pid in members:
        if pid != me:
            try:
                os.kill(pid, sig)
            except OSError:
                pass
    return True

def finish(proc, rc):
    # Polite signal, bounded grace, then SIGKILL. A membership probe that
    # cannot answer counts as survival. Skip this process: it is the reaper.
    signal_others(signal.SIGTERM)
    if rc is None:
        try:
            rc = proc.wait(timeout=0.5)
        except subprocess.TimeoutExpired:
            signal_others(signal.SIGKILL)
            rc = proc.wait(timeout=1)
    deadline = time.monotonic() + 0.5
    while time.monotonic() < deadline and others_alive():
        reap_zombies()
        time.sleep(0.02)
    if others_alive():
        signal_others(signal.SIGKILL)
        deadline = time.monotonic() + 0.5
        while time.monotonic() < deadline and others_alive():
            reap_zombies()
            time.sleep(0.02)
    reap_zombies()
    return rc

def handle_term(_signum, _frame):
    # The Rust guard already sent SIGTERM to the group. Reap what died, then
    # SIGKILL any descendant that ignored it, before this process exits.
    deadline = time.monotonic() + 0.4
    while time.monotonic() < deadline and others_alive():
        reap_zombies()
        time.sleep(0.02)
    signal_others(signal.SIGKILL)
    deadline = time.monotonic() + 0.4
    while time.monotonic() < deadline and others_alive():
        reap_zombies()
        time.sleep(0.02)
    reap_zombies()
    sys.stdout.flush()
    sys.stderr.flush()
    os._exit(143)

signal.signal(signal.SIGTERM, handle_term)
boundary = os.environ.get("QA_BOUNDARY", "")
start = time.monotonic()
proc = subprocess.Popen(["bash", script])
rc = None
timed_out = 0
while True:
    try:
        rc = proc.wait(timeout=0.05)
        break
    except subprocess.TimeoutExpired:
        # Once the parent has recorded the held child, do not wait out the
        # recipe's readiness loop. That is the timeout under test.
        if boundary == "timeout" and os.path.isfile(observed_pid_file) and os.path.getsize(observed_pid_file) > 0:
            timed_out = 1
            break
        if time.monotonic() - start > 10:
            timed_out = 1
            break
observed = read_pid(observed_pid_file)
published = read_pid(child_pid_file)
descendant = read_pid(descendant_pid_file)
child = observed or published
child_state = classify(child)
descendant_state = classify(descendant)
rc = finish(proc, rc)
reap_zombies()

def after(pid):
    if not pid:
        return "missing"
    return "gone" if proc_state(pid) is None else "present"

print(f"harness_timeout={timed_out}")
print(f"harness_rc={rc}")
print(f"elapsed_ms={int((time.monotonic() - start) * 1000)}")
print(f"observed_pid={observed or ''}")
print(f"published_pid={published or ''}")
print(f"child_pid={child or ''}")
print(f"child_state={child_state}")
print(f"descendant_pid={descendant or ''}")
print(f"descendant_state={descendant_state}")
print(f"child_after={after(child)}")
print(f"descendant_after={after(descendant)}")
print(f"group_survived={1 if others_alive() else 0}")
sys.stdout.flush()
"#;
    let child_pid_file = qa_base.join("child.pid");
    let observed_pid_file = qa_base.join("observed.pid");
    let descendant_pid_file = qa_base.join("descendant.pid");
    make_fifo(&qa_base.join("hold.fifo"));
    make_fifo(&qa_base.join("ready.fifo"));
    let mut command = Command::new("/usr/bin/python3");
    command
        .arg("-c")
        .arg(RUNNER)
        .arg(&script_path)
        .arg(&child_pid_file)
        .arg(&observed_pid_file)
        .arg(&descendant_pid_file)
        .env("QA_BASE", &qa_base)
        .env("QA_HOME", temp.path().join("home"))
        .env("QA_REPO", &qa_repo)
        .env("QA_BOUNDARY", boundary)
        .env("QA_TERM_GRACE", "5")
        .env("ORBIT_BIN", "/bin/true")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut runner = OwnedRunner::spawn(&mut command);
    let started = Instant::now();
    let status = runner
        .wait_finished(Duration::from_secs(25))
        .unwrap_or_else(|| panic!("{boundary}: subreaper runner did not finish"));
    let (stdout, stderr) = runner.output_text();
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
    let expect_timeout = if boundary == "timeout" { "1" } else { "0" };
    assert_eq!(
        field("harness_timeout="),
        expect_timeout,
        "{boundary}: unexpected timeout flag: {stdout} {stderr}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "{boundary}: harness took too long: {stdout}"
    );
    // The timeout sweep stops the recipe before its epilogue can report the
    // caller's job. Every path that returns to the outer script must leave it.
    if boundary != "timeout" {
        assert_eq!(
            field("caller_job="),
            "alive",
            "{boundary}: the caller's own background job must be left untouched: {stdout} {stderr}"
        );
    }
    let expected_rc = match boundary {
        "none" => Some("0"),
        "unreaped" => Some("137"),
        "timeout" => None,
        _ => Some("143"),
    };
    if let Some(expected_rc) = expected_rc {
        assert_eq!(
            field("harness_rc="),
            expected_rc,
            "{boundary}: unexpected harness exit: {stdout} {stderr}"
        );
    }
    let expected_child = match boundary {
        "before_spawn" => "missing",
        "unreaped" | "timeout" => "alive",
        _ => "reaped",
    };
    assert_eq!(
        field("child_state="),
        expected_child,
        "{boundary}: unexpected child state: {stdout} {stderr}"
    );
    let expected_after = if boundary == "before_spawn" {
        "missing"
    } else {
        "gone"
    };
    assert_eq!(
        field("child_after="),
        expected_after,
        "{boundary}: child was not reaped: {stdout} {stderr}"
    );
    let expected_descendant = if matches!(boundary, "unreaped" | "timeout") {
        "alive"
    } else {
        "missing"
    };
    assert_eq!(
        field("descendant_state="),
        expected_descendant,
        "{boundary}: unexpected descendant state: {stdout} {stderr}"
    );
    let expected_descendant_after = if matches!(boundary, "unreaped" | "timeout") {
        "gone"
    } else {
        "missing"
    };
    assert_eq!(
        field("descendant_after="),
        expected_descendant_after,
        "{boundary}: descendant was not reaped: {stdout} {stderr}"
    );
    assert_eq!(
        field("group_survived="),
        "0",
        "{boundary}: owned process group still has a member: {stdout} {stderr}"
    );
    // The held stub must not publish. The normal run publishes the same PID
    // the parent recorded.
    let published_on_disk = std::fs::read_to_string(&child_pid_file)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
    if boundary == "none" {
        assert_eq!(
            field("published_pid="),
            field("observed_pid="),
            "{boundary}: parent and child disagree on the server pid: {stdout} {stderr}"
        );
        assert!(
            published_on_disk.is_some(),
            "{boundary}: normal stub did not publish its pid: {stdout}"
        );
    } else {
        assert_eq!(
            field("published_pid="),
            "",
            "{boundary}: stub published a pid the fixture must not need: {stdout} {stderr}"
        );
        assert!(
            published_on_disk.is_none(),
            "{boundary}: pid file exists: {published_on_disk:?}"
        );
    }
    if boundary != "before_spawn" {
        let child_pid = field("child_pid=");
        let caller_pid = field("caller_job_pid=");
        assert!(
            !child_pid.is_empty() && child_pid != caller_pid,
            "{boundary}: expected the spawned server pid, distinct from the caller job {caller_pid}: {stdout} {stderr}"
        );
        let child_pid: libc::pid_t = child_pid.parse().expect("server pid");
        assert_pid_gone(child_pid, &format!("{boundary} server"));
    }
    if matches!(boundary, "unreaped" | "timeout") {
        let descendant_pid: libc::pid_t = field("descendant_pid=").parse().expect("descendant pid");
        assert_pid_gone(descendant_pid, &format!("{boundary} descendant"));
    }
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

/// Own the QA runner and every descendant in its process group.
///
/// Drop and the deadline path send SIGTERM while this child is still unreaped,
/// so the group id cannot have been reused, then SIGKILL only if it is still
/// the live leader. The runner reaps descendants before it exits on SIGTERM.
#[cfg(target_os = "linux")]
struct OwnedRunner {
    child: Child,
}

#[cfg(target_os = "linux")]
impl OwnedRunner {
    fn spawn(command: &mut Command) -> Self {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        let child = command.spawn().expect("spawn subreaper runner");
        Self { child }
    }

    fn terminate(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
        let pgid = self.child.id() as libc::pid_t;
        if pgid <= 1 {
            return;
        }
        // SAFETY: `pgid` is the pid of this unreaped child, which leads the
        // group created at spawn. `kill(-pgid)` signals only that group.
        unsafe { libc::kill(-pgid, libc::SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => break,
            }
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            // SAFETY: the leader is still unreaped, so the group id is still
            // the one this fixture created.
            unsafe { libc::kill(-pgid, libc::SIGKILL) };
            let _ = self.child.wait();
        }
    }

    fn wait_finished(&mut self, deadline: Duration) -> Option<std::process::ExitStatus> {
        let start = Instant::now();
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            if start.elapsed() >= deadline {
                self.terminate();
                return self.child.try_wait().ok().flatten();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn output_text(&mut self) -> (String, String) {
        fn read_capped(pipe: impl Read) -> String {
            let mut text = String::new();
            let _ = pipe.take(256 * 1024).read_to_string(&mut text);
            text
        }
        let stdout = self
            .child
            .stdout
            .take()
            .map(read_capped)
            .unwrap_or_default();
        let stderr = self
            .child
            .stderr
            .take()
            .map(read_capped)
            .unwrap_or_default();
        (stdout, stderr)
    }
}

#[cfg(target_os = "linux")]
impl Drop for OwnedRunner {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(target_os = "linux")]
fn make_fifo(path: &std::path::Path) {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = path.as_os_str().as_bytes().to_vec();
    bytes.push(0);
    // SAFETY: `bytes` is a NUL-terminated path this fixture owns, with no
    // interior NUL. `mkfifo` creates that FIFO and nothing else.
    let rc = unsafe { libc::mkfifo(bytes.as_ptr().cast(), 0o600) };
    assert_eq!(
        rc,
        0,
        "mkfifo {}: {}",
        path.display(),
        std::io::Error::last_os_error()
    );
}

#[cfg(target_os = "linux")]
fn assert_pid_gone(pid: libc::pid_t, context: &str) {
    assert!(pid > 1, "{context}: pid {pid} is not a fixture child");
    // SAFETY: signal 0 only probes this fixture's recorded pid.
    let kill_rc = unsafe { libc::kill(pid, 0) };
    let kill_err = std::io::Error::last_os_error().raw_os_error();
    assert_eq!(kill_rc, -1, "{context}: pid {pid} still exists");
    assert_eq!(
        kill_err,
        Some(libc::ESRCH),
        "{context}: pid {pid} probe returned {kill_err:?}"
    );
    // SAFETY: WNOHANG wait of a pid this fixture spawned. It does not block,
    // and ECHILD means this process does not still have it as a zombie.
    let wait_rc = unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
    let wait_err = std::io::Error::last_os_error().raw_os_error();
    assert_eq!(wait_rc, -1, "{context}: pid {pid} was left unreaped");
    assert_eq!(
        wait_err,
        Some(libc::ECHILD),
        "{context}: pid {pid} reap probe returned {wait_err:?}"
    );
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
