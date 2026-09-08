//! Parent SIGINT/SIGTERM must not be swallowed while Orbit supervises a child
//! (ORB-11697).
//!
//! `SignalHandlerGuard` intercepts those signals so the child's process group
//! can be torn down. After the last waiter restores the previous disposition
//! it re-raises, so `orbit mcp listen` (SIG_DFL) and an interactive CLI
//! (SIGINT) still exit instead of running forever.

#![allow(missing_docs)]
#![cfg(unix)]
// Integration fixtures use expect/unwrap for concise failure diagnostics.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use orbit_common::test_env;
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};

/// Upper bound between SIGTERM and process exit. The child's own termination
/// grace period is 5s (`TERMINATION_GRACE_PERIOD`); this is only a CI jitter
/// ceiling, well below systemd's typical 90s `TimeoutStopUSec`.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);
const STARTUP_DEADLINE: Duration = Duration::from_secs(15);
const MARKER_DEADLINE: Duration = Duration::from_secs(8);

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn init() -> Self {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&home).expect("create home");
        std::fs::create_dir_all(&work).expect("create work");

        let output = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&work)
            .output()
            .expect("git init");
        assert!(output.status.success(), "git init failed: {output:?}");

        let output = orbit_command(&work, &home)
            .args([
                "init",
                "--non-interactive",
                "--host-name",
                "signal-host",
                "--task-prefix",
                "SIG",
            ])
            .output()
            .expect("orbit init");
        assert!(
            output.status.success(),
            "orbit init failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let output = orbit_command(&work, &home)
            .args(["workspace", "init", "--name", "signal-ws"])
            .output()
            .expect("workspace init");
        assert!(
            output.status.success(),
            "workspace init failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        Self {
            _temp: temp,
            home,
            work,
        }
    }
}

#[test]
fn mcp_listen_exits_on_sigterm_while_proc_spawn_runs() {
    let fixture = Fixture::init();
    let addr = free_loopback_addr();
    let mut server = spawn_mcp_listen(&fixture, addr);
    wait_for_listening(addr);

    let stream = TcpStream::connect(addr).expect("connect to mcp listen");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    let mut reader = stream.try_clone().expect("clone socket");
    let mut writer = stream;
    mcp_initialize(&mut writer, &mut reader, &fixture.work);

    let marker = fixture.work.join("listen.ready");
    let call = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "proc.spawn",
            "arguments": {
                "program": "/bin/sh",
                "args": ["-c", format!("touch {} && sleep 30", marker.display())],
                "timeout_ms": 60_000
            }
        }
    });
    send_rpc(&mut writer, &call);
    let advertised = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "proc_spawn",
            "arguments": {
                "program": "/bin/sh",
                "args": ["-c", format!("touch {} && sleep 30", marker.display())],
                "timeout_ms": 60_000
            }
        }
    });
    send_rpc(&mut writer, &advertised);

    let listen_has_child = wait_for_marker_optional(&marker, MARKER_DEADLINE);
    // `proc.spawn` is a CLI/activity tool, not MCP-advertised. When the
    // listen process cannot host the child, drive the same
    // SignalHandlerGuard contract through `orbit tool run proc.spawn`.
    let mut cli_child = if listen_has_child {
        None
    } else {
        drop(writer);
        drop(reader);
        Some(spawn_cli_proc_spawn(&fixture, &marker))
    };

    let before = Instant::now();
    if listen_has_child {
        send_signal(&server, libc::SIGTERM);
        let status = wait_with_deadline(&mut server, SHUTDOWN_DEADLINE).unwrap_or_else(|| {
            panic!(
                "orbit mcp listen did not exit within {SHUTDOWN_DEADLINE:?} of SIGTERM \
                 while supervising proc.spawn"
            )
        });
        assert_signaled_or_nonzero(&status, libc::SIGTERM);
    } else {
        let child = cli_child.as_mut().expect("cli proc.spawn child");
        send_signal(child, libc::SIGTERM);
        let status = wait_with_deadline(child, SHUTDOWN_DEADLINE).unwrap_or_else(|| {
            panic!("orbit tool run proc.spawn did not exit within {SHUTDOWN_DEADLINE:?} of SIGTERM")
        });
        assert_signaled_or_nonzero(&status, libc::SIGTERM);
        send_signal(&server, libc::SIGTERM);
        let _ = wait_with_deadline(&mut server, SHUTDOWN_DEADLINE);
    }
    assert!(
        before.elapsed() < SHUTDOWN_DEADLINE,
        "SIGTERM shutdown took {:?}",
        before.elapsed()
    );
}

#[test]
fn cli_ctrl_c_during_proc_spawn_reports_interrupt() {
    let fixture = Fixture::init();
    let marker = fixture.work.join("cli.ready");
    let mut child = spawn_cli_proc_spawn(&fixture, &marker);
    wait_for_marker(&marker);

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    send_signal(&child, libc::SIGINT);
    let status = wait_with_deadline(&mut child, SHUTDOWN_DEADLINE).unwrap_or_else(|| {
        panic!("orbit tool run proc.spawn did not exit within {SHUTDOWN_DEADLINE:?} of SIGINT")
    });
    assert_signaled_or_nonzero(&status, libc::SIGINT);

    let mut stderr = String::new();
    if let Some(ref mut pipe) = stderr_pipe {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let mut stdout = String::new();
    if let Some(ref mut pipe) = stdout_pipe {
        let _ = pipe.read_to_string(&mut stdout);
    }
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("process interrupted by signal SIGINT"),
        "expected the CLI interrupt message, got stdout={stdout:?} stderr={stderr:?}"
    );
}

fn orbit_command(work: &Path, home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit"));
    test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .current_dir(work)
        .env("HOME", home)
        .env("USERPROFILE", home);
    command
}

fn spawn_mcp_listen(fixture: &Fixture, addr: SocketAddr) -> Child {
    orbit_command(&fixture.work, &fixture.home)
        .args(["mcp", "listen", &addr.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn orbit mcp listen")
}

fn spawn_cli_proc_spawn(fixture: &Fixture, marker: &Path) -> Child {
    let input = json!({
        "program": "/bin/sh",
        "args": ["-c", format!("touch {} && sleep 30", marker.display())],
        "timeout_ms": 60_000
    });
    let child = orbit_command(&fixture.work, &fixture.home)
        .args(["tool", "run", "proc.spawn", "--input", &input.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn orbit tool run proc.spawn");
    wait_for_marker(marker);
    child
}

fn free_loopback_addr() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .expect("probe a free loopback port")
        .local_addr()
        .expect("probe address")
}

fn wait_for_listening(addr: SocketAddr) {
    let deadline = Instant::now() + STARTUP_DEADLINE;
    loop {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("orbit mcp listen did not bind {addr} within {STARTUP_DEADLINE:?}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn mcp_initialize(writer: &mut TcpStream, reader: &mut TcpStream, workspace: &Path) {
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "orb-11697", "version": "0" },
            "_meta": { "orbit": { "workspace": workspace.to_str().expect("utf8 workspace") } }
        }
    });
    send_rpc(writer, &initialize);
    let response = read_rpc_line(reader).expect("initialize response");
    assert_eq!(
        response["result"]["protocolVersion"], "2025-06-18",
        "initialize failed: {response}"
    );
    send_rpc(
        writer,
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    );
}

fn send_rpc(writer: &mut TcpStream, message: &Value) {
    let mut line = serde_json::to_string(message).expect("serialize rpc");
    line.push('\n');
    writer.write_all(line.as_bytes()).expect("write rpc");
    writer.flush().expect("flush rpc");
}

fn read_rpc_line(reader: &mut TcpStream) -> Option<Value> {
    let mut line = String::new();
    BufReader::new(reader).read_line(&mut line).ok()?;
    if line.trim().is_empty() {
        return None;
    }
    serde_json::from_str(line.trim()).ok()
}

fn wait_for_marker(path: &Path) {
    if !wait_for_marker_optional(path, MARKER_DEADLINE) {
        panic!("child did not become ready: {}", path.display());
    }
}

fn wait_for_marker_optional(path: &Path, deadline: Duration) -> bool {
    let end = Instant::now() + deadline;
    while Instant::now() < end {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    path.exists()
}

fn send_signal(child: &Child, signal: i32) {
    // Safety: `kill` targets this test's already-spawned child with the
    // signal under test and performs no other side effect.
    let rc = unsafe { libc::kill(child.id() as libc::pid_t, signal) };
    assert_eq!(
        rc,
        0,
        "failed to send signal {signal}: {}",
        std::io::Error::last_os_error()
    );
}

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

fn assert_signaled_or_nonzero(status: &std::process::ExitStatus, signal: i32) {
    if status.signal() == Some(signal) {
        return;
    }
    assert!(
        !status.success(),
        "expected SIG{signal} or a non-zero exit, got {status:?}"
    );
}
