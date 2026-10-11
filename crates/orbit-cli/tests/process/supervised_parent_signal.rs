//! Parent SIGINT/SIGTERM must not be swallowed while Orbit supervises a child
//! (ORB-11697).
//!
//! `SignalHandlerGuard` intercepts those signals so the child's process group
//! can be torn down. After the last waiter restores the previous disposition
//! it re-raises, so `orbit mcp listen` (SIG_DFL) and an interactive CLI
//! (SIGINT) still exit instead of running forever. The listener test calls an
//! advertised exec plugin because the TCP listener has agent authority only;
//! the operator-only `orbit_command_exec` cannot run on that transport.

#![allow(missing_docs)]
#![cfg(any(target_os = "linux", target_os = "macos"))]
// Integration fixtures use expect/unwrap for concise failure diagnostics.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::ops::{Deref, DerefMut};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use orbit_common::test_env;
use serde_json::{Value, json};
use tempfile::{Builder, TempDir};

#[path = "backend_guard.rs"]
mod backend_guard;
use backend_guard::{BackendGuard, BackendReadiness};

const BACKEND_ENTRY: &str = "supervised_parent_signal::signal_fixture_backend";

/// Upper bound between SIGTERM and process exit. The child's own termination
/// grace period is 5s (`TERMINATION_GRACE_PERIOD`); this is only a CI jitter
/// ceiling, well below systemd's typical 90s `TimeoutStopUSec`.
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(10);
// Startup can be slow under the box lane's concurrent suite (ORB-14396).
// Only readiness uses this ceiling; signal handling retains its 10s bound.
const STARTUP_DEADLINE: Duration = Duration::from_secs(60);

struct ChildGuard(Child);

impl Deref for ChildGuard {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.0
    }
}

impl DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        stop_child(&mut self.0);
    }
}

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn init() -> Self {
        // Exercise the positional marker argument with a non-shell-safe path.
        let temp = Builder::new()
            .prefix("signal fixture ")
            .tempdir()
            .expect("tempdir");
        let home = temp.path().join("home");
        let work = temp.path().join("work");
        std::fs::create_dir_all(&home).expect("create home");
        std::fs::create_dir_all(&work).expect("create work");

        let output = crate::git_repo::command()
            .args(["init", "--quiet"])
            .current_dir(&work)
            .output()
            .expect("git init");
        assert!(output.status.success(), "git init failed: {output:?}");

        let output = orbit_command(&work, &home)
            .args([
                "init",
                "--non-interactive",
                "--machine-name",
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
fn mcp_listen_exits_on_sigterm_while_plugin_child_runs() {
    let fixture = Fixture::init();
    install_signal_plugin(&fixture);
    let addr = free_loopback_addr();
    let mut server = spawn_mcp_listen(&fixture, addr);
    wait_for_listening(&mut server, addr);

    let stream = TcpStream::connect(addr).expect("connect to mcp listen");
    stream
        .set_read_timeout(Some(STARTUP_DEADLINE))
        .expect("read timeout");
    let mut reader = stream.try_clone().expect("clone socket");
    let mut writer = stream;
    mcp_initialize(&mut writer, &mut reader, &fixture.work);

    send_rpc(
        &mut writer,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list"
        }),
    );
    let listed = read_rpc_line(&mut reader).expect("tools/list response");
    let tools = listed["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools/list failed: {listed}"));
    assert!(
        tools
            .iter()
            .any(|tool| tool["name"] == "signalfixture_wait"),
        "the supervising plugin tool must be advertised: {listed}"
    );
    if !orbit_exec::macos_sandbox_test_guard("mcp_listen_exits_on_sigterm_while_plugin_child_runs")
    {
        return;
    }
    // The backend receives Orbit's canonical root; the receiver must identify
    // the same workspace through a symlink, as temporary roots may contain one.
    let readiness_workspace = fixture
        .work
        .parent()
        .expect("fixture root")
        .join("work alias");
    std::os::unix::fs::symlink(&fixture.work, &readiness_workspace)
        .expect("readiness workspace alias");
    let readiness =
        BackendReadiness::new(&readiness_workspace).expect("backend readiness receiver");
    send_rpc(
        &mut writer,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": { "name": "signalfixture_wait", "arguments": {} }
        }),
    );
    // Only this in-flight MCP call can announce readiness. Keep its connection
    // open and verify the retained backend identity before signalling its parent.
    let backend = wait_for_backend(&mut server, &readiness, &mut reader);
    assert!(
        backend.is_live().expect("probe backend identity"),
        "plugin child must be live"
    );

    let before = Instant::now();
    send_signal(&server, libc::SIGTERM);
    let status = wait_with_deadline(&mut server, SHUTDOWN_DEADLINE).unwrap_or_else(|| {
        panic!(
            "orbit mcp listen did not exit within {SHUTDOWN_DEADLINE:?} of SIGTERM \
             while supervising a plugin tools/call"
        )
    });
    assert_eq!(
        status.signal(),
        Some(libc::SIGTERM),
        "listener must restore the SIGTERM disposition, got {status:?}"
    );
    assert!(
        before.elapsed() < SHUTDOWN_DEADLINE,
        "SIGTERM shutdown took {:?}",
        before.elapsed()
    );
    let deadline = Instant::now() + SHUTDOWN_DEADLINE;
    while backend.is_live().expect("probe backend identity") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !backend.is_live().expect("probe backend identity"),
        "supervised plugin child {} survived listener shutdown",
        backend.identity.pid
    );
}

/// A real plugin backend, re-executed from the process integration binary.
/// Readiness carries its own identity, closing the stale-PID acquisition window.
#[test]
#[ignore = "child entry point for the signal plugin fixture"]
fn signal_fixture_backend() {
    let workspace = std::env::var_os("ORBIT_WORKSPACE_ROOT").expect("fixture workspace");
    let _lease = backend_guard::publish_readiness(Path::new(&workspace))
        .expect("publish backend's own identity");
    // No exec after recording identity: macOS pidversion changes across exec.
    std::thread::sleep(Duration::from_secs(120));
}

fn wait_for_backend(
    server: &mut Child,
    readiness: &BackendReadiness,
    reader: &mut TcpStream,
) -> BackendGuard {
    let deadline = Instant::now() + STARTUP_DEADLINE;
    loop {
        if let Some(backend) = readiness
            .receive()
            .expect("receive backend identity handle")
        {
            return backend;
        }
        if server.try_wait().expect("poll listener").is_some() || Instant::now() >= deadline {
            reader
                .set_read_timeout(Some(Duration::from_millis(100)))
                .expect("diagnostic timeout");
            let response = read_rpc_line(reader);
            fail_startup(
                server,
                &format!("MCP plugin call did not write its child identity; response={response:?}"),
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn install_signal_plugin(fixture: &Fixture) {
    // Install solely into the disposable HOME, outside the fixture checkout.
    // This uses the real advertised plugin surface and its normal sandbox
    // grants without changing the listener's agent-only authority. The tool
    // only waits; its readiness marker is fixture instrumentation.
    let source = fixture.home.join("plugin-source/.orbit-plugin");
    std::fs::create_dir_all(source.join("bin")).expect("create plugin source");
    // The granted write root must exist before the sandboxed backend writes
    // its readiness marker into it.
    std::fs::create_dir_all(fixture.work.join("markers")).expect("create marker dir");
    test_env::assert_child_test_exists(BACKEND_ENTRY);
    // The plugin's normal read/exec grant covers its own installed tree, so
    // copy the child entry point there rather than widen its sandbox grants.
    let backend = source.join("bin/wait");
    std::fs::copy(
        std::env::current_exe().expect("process integration binary"),
        &backend,
    )
    .expect("copy plugin backend");
    // Assess the new executable before the timed listener startup on macOS.
    let mut probe = Command::new(&backend);
    test_env::clear_inherited_authority(|name| {
        probe.env_remove(name);
    });
    probe.args(["--list", "--exact", BACKEND_ENTRY, "--include-ignored"]);
    let output = orbit_common::process::run_bounded_capped(&mut probe, STARTUP_DEADLINE, 64 * 1024)
        .expect("assess copied backend");
    assert!(
        output.status.success(),
        "backend assessment failed: {output:?}"
    );
    std::fs::write(
        source.join("plugin.yaml"),
        r#"schemaVersion: 2
kind: Plugin
metadata:
  name: signalfixture
  version: 0.1.0
  description: Long-lived supervised signal fixture.
spec:
  permissions:
    fs:
      write: ['{{workspace}}/markers']
  backend:
    type: exec
    command: bin/wait
    args: ['--ignored', '--exact', 'supervised_parent_signal::signal_fixture_backend', '--nocapture']
    timeout_ms: 120000
  tools:
    - name: wait
      description: Wait for the parent signal.
      execution_kind: read_only
      mcp_scope: workspace
      input_schema:
        type: object
"#,
    )
    .expect("write plugin manifest");
    let output = orbit_command(&fixture.work, &fixture.home)
        .args(["plugin", "add"])
        .arg(&source)
        .args(["--enable", "--grant", "fs={{workspace}}/markers"])
        .output()
        .expect("install signal plugin");
    assert!(
        output.status.success(),
        "plugin install failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_ctrl_c_during_proc_spawn_reports_interrupt() {
    // `proc.spawn` confines its child with Seatbelt on macOS, which a nested
    // agent sandbox refuses to apply [ORB-15409].
    if !orbit_exec::macos_sandbox_test_guard("cli_ctrl_c_during_proc_spawn_reports_interrupt") {
        return;
    }
    let fixture = Fixture::init();
    let marker = fixture.work.join("cli.ready");
    let mut child = spawn_cli_proc_spawn(&fixture, &marker);

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

fn spawn_mcp_listen(fixture: &Fixture, addr: SocketAddr) -> ChildGuard {
    ChildGuard(
        orbit_command(&fixture.work, &fixture.home)
            .args(["mcp", "listen", &addr.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn orbit mcp listen"),
    )
}

fn proc_spawn_input(marker: &Path) -> Value {
    // The marker belongs to the fixture, not a global /tmp name. A shell
    // builtin writes it without PATH lookup, and a positional argument keeps
    // spaces or shell metacharacters in TMPDIR from changing the command.
    json!({
        "program": "/bin/sh",
        "args": ["-c", "printf '%s\\n' ready > \"$1\" && sleep 30", "orbit-signal-fixture", marker],
        "timeout_ms": 60_000
    })
}

fn spawn_cli_proc_spawn(fixture: &Fixture, marker: &Path) -> ChildGuard {
    let input = proc_spawn_input(marker);
    let mut child = ChildGuard(
        orbit_command(&fixture.work, &fixture.home)
            .args(["tool", "run", "proc.spawn", "--input", &input.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn orbit tool run proc.spawn"),
    );
    wait_for_marker(&mut child, marker);
    child
}

fn free_loopback_addr() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .expect("probe a free loopback port")
        .local_addr()
        .expect("probe address")
}

fn wait_for_listening(child: &mut Child, addr: SocketAddr) {
    let deadline = Instant::now() + STARTUP_DEADLINE;
    loop {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        if child.try_wait().expect("poll mcp listen").is_some() || Instant::now() >= deadline {
            fail_startup(child, &format!("orbit mcp listen did not bind {addr}"));
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

fn wait_for_marker(child: &mut Child, path: &Path) {
    let end = Instant::now() + STARTUP_DEADLINE;
    loop {
        if child.try_wait().expect("poll supervisor").is_some() || Instant::now() >= end {
            fail_startup(
                child,
                &format!("child did not become ready: {}", path.display()),
            );
        }
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn stop_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        // Safety: this is the test's owned child. SIGTERM lets the supervisor
        // clean up its child group even when a readiness assertion panics.
        let _ = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
        let _ = wait_with_deadline(child, SHUTDOWN_DEADLINE);
    }
}

fn fail_startup(child: &mut Child, message: &str) -> ! {
    stop_child(child);
    let status = child.try_wait().expect("reaped supervisor status");
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    panic!(
        "{message} within {STARTUP_DEADLINE:?}; status={status:?}\nstdout: {stdout}\nstderr: {stderr}"
    );
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
