#![allow(missing_docs)]
#![cfg(unix)]
// Integration fixtures exercise public behavior and unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Ctrl-C must terminate every live supervised process group, not only the
//! one that happened to hold the (former) process-wide handler mutex.
//!
//! This lives in its own test binary so `raise(SIGINT)` cannot interrupt
//! other `orbit-exec` unit tests sharing a process.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};

static FORWARDED_SIGNAL: AtomicI32 = AtomicI32::new(0);
static TEST_LOCK: Mutex<()> = Mutex::new(());

unsafe extern "C" fn record_previous_handler(signal: libc::c_int) {
    FORWARDED_SIGNAL.store(signal, Ordering::SeqCst);
}

/// In-process tests must not restore SIG_DFL: last-drop re-raise would then
/// terminate the test binary. Install a recorder that stands in for tokio's
/// previous handler.
fn install_previous_handler(signal: libc::c_int) {
    // Safety: test-only `sigaction` for a handler that stores one atomic.
    unsafe {
        let mut new_action: libc::sigaction = std::mem::zeroed();
        new_action.sa_sigaction = record_previous_handler as *const () as usize;
        new_action.sa_flags = 0;
        libc::sigemptyset(&mut new_action.sa_mask);
        let rc = libc::sigaction(signal, &new_action, std::ptr::null_mut());
        assert_eq!(rc, 0, "install previous handler");
    }
}

#[test]
fn ctrl_c_terminates_every_live_child_process_group() {
    let _lock = TEST_LOCK.lock().expect("signal test lock");
    FORWARDED_SIGNAL.store(0, Ordering::SeqCst);
    install_previous_handler(libc::SIGINT);

    let dir = tempfile::tempdir().expect("tempdir");
    let first_ready = dir.path().join("first.ready");
    let second_ready = dir.path().join("second.ready");

    let started = Instant::now();
    thread::scope(|scope| {
        let first = scope.spawn(|| supervise_sleep_after_ready(&first_ready));
        let second = scope.spawn(|| supervise_sleep_after_ready(&second_ready));
        wait_for_marker(&first_ready);
        wait_for_marker(&second_ready);

        // Safety: `raise` delivers SIGINT to this process. The supervised
        // wait has replaced the previous disposition with Orbit's handler.
        let raised = unsafe { libc::raise(libc::SIGINT) };
        assert_eq!(raised, 0, "raise SIGINT");

        let first = first.join().expect("first supervisor thread");
        let second = second.join().expect("second supervisor thread");
        assert_interrupted(&first, libc::SIGINT);
        assert_interrupted(&second, libc::SIGINT);
    });

    assert_eq!(
        FORWARDED_SIGNAL.load(Ordering::SeqCst),
        libc::SIGINT,
        "previous SIGINT handler must run after the child is reaped"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "SIGINT should terminate both live children promptly, took {:?}",
        started.elapsed()
    );
}

#[test]
fn sigterm_is_forwarded_to_the_previous_handler() {
    let _lock = TEST_LOCK.lock().expect("signal test lock");
    FORWARDED_SIGNAL.store(0, Ordering::SeqCst);
    install_previous_handler(libc::SIGTERM);

    let dir = tempfile::tempdir().expect("tempdir");
    let ready = dir.path().join("term.ready");

    thread::scope(|scope| {
        let waiter = scope.spawn(|| supervise_sleep_after_ready(&ready));
        wait_for_marker(&ready);

        // Safety: `raise` delivers SIGTERM to this process while Orbit's
        // supervisor owns the disposition; last drop must restore and
        // re-raise into the recorder installed above.
        let raised = unsafe { libc::raise(libc::SIGTERM) };
        assert_eq!(raised, 0, "raise SIGTERM");

        let result = waiter.join().expect("supervisor thread");
        assert_interrupted(&result, libc::SIGTERM);
    });

    assert_eq!(
        FORWARDED_SIGNAL.load(Ordering::SeqCst),
        libc::SIGTERM,
        "previous SIGTERM handler must run after the child is reaped"
    );
}

const SIGTERM_HELPER_ENV: &str = "ORBIT_EXEC_SIGTERM_HELPER";
const SIGTERM_MARKER_ENV: &str = "ORBIT_EXEC_SIGTERM_MARKER";

/// SIGTERM with the default disposition must actually exit the supervisor
/// process (the `orbit mcp listen` / systemd-stop case).
#[test]
fn sigterm_with_default_disposition_exits_the_supervisor() {
    if std::env::var_os(SIGTERM_HELPER_ENV).is_some() {
        let marker =
            PathBuf::from(std::env::var(SIGTERM_MARKER_ENV).expect("marker dir")).join("ready");
        let _ = supervise_sleep_after_ready(&marker);
        panic!("supervisor returned instead of dying on SIGTERM");
    }

    let _lock = TEST_LOCK.lock().expect("signal test lock");
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("ready");
    let exe = std::env::current_exe().expect("current test binary");
    let mut child = Command::new(&exe)
        .env(SIGTERM_HELPER_ENV, "1")
        .env(SIGTERM_MARKER_ENV, dir.path())
        .args([
            "--exact",
            "sigterm_with_default_disposition_exits_the_supervisor",
            "--nocapture",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn supervisor helper");

    wait_for_marker(&marker);
    // Safety: SIGTERM targets this test's helper pid — the same signal
    // systemd sends on stop — and performs no other side effect.
    let rc = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(rc, 0, "kill helper with SIGTERM");

    let started = Instant::now();
    let output = wait_child_output(&mut child, Duration::from_secs(8)).unwrap_or_else(|| {
        panic!("supervisor helper did not exit within the termination grace period")
    });
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "SIGTERM should exit the supervisor within the child-termination grace period, took {:?}",
        started.elapsed()
    );
    assert!(
        !output.status.success(),
        "SIGTERM must exit the supervisor non-zero, got {:?}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("process interrupted by signal SIGTERM"),
        "stderr was {stderr:?}"
    );
}

fn supervise_sleep_after_ready(marker: &Path) -> orbit_exec::ExecutionResult {
    let script = format!("touch {} && sleep 8", marker.display());
    let req = ExecRequest {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script],
        current_dir: None,
        timeout_ms: Some(15_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::Inherit,
        debug: false,
    };
    run_process(&req, &NoSandbox).expect("run_process")
}

fn wait_for_marker(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("child did not become ready: {}", path.display());
}

fn assert_interrupted(result: &orbit_exec::ExecutionResult, signal: i32) {
    let name = match signal {
        libc::SIGINT => "SIGINT",
        libc::SIGTERM => "SIGTERM",
        _ => "UNKNOWN",
    };
    assert!(!result.success, "interrupted child must not succeed");
    assert_eq!(
        result.exit_code,
        Some(128 + signal),
        "parent-signal exits report 128+{name}, got {:?}",
        result.exit_code
    );
    let expected = format!("process interrupted by signal {name}");
    assert!(
        result.stderr.contains(&expected),
        "stderr was {:?}",
        result.stderr
    );
}

fn wait_child_output(
    child: &mut std::process::Child,
    deadline: Duration,
) -> Option<std::process::Output> {
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let stderr_thread = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stderr_pipe, &mut buf);
        buf
    });
    let start = Instant::now();
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break status;
        }
        if start.elapsed() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    Some(std::process::Output {
        status,
        stdout: Vec::new(),
        stderr: stderr_thread.join().unwrap_or_default(),
    })
}
