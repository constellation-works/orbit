#![allow(missing_docs)]
#![cfg(unix)]
// Integration fixtures exercise public behavior and unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Ctrl-C must terminate every live supervised process group, not only the
//! one that happened to hold the (former) process-wide handler mutex.
//!
//! This lives in its own test binary so `raise(SIGINT)` cannot interrupt
//! other `orbit-exec` unit tests sharing a process.

use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_exec::{
    EnvironmentMode, ExecRequest, NoSandbox, Sandbox, StdinMode, run_process,
    run_process_streaming_stdout,
};
use wait_timeout::ChildExt;

static FORWARDED_SIGNAL: AtomicI32 = AtomicI32::new(0);
static FORWARD_PROBE_PID: AtomicI32 = AtomicI32::new(0);
static FORWARDED_CHILD_PROBE: AtomicI32 = AtomicI32::new(0);
static TEST_LOCK: Mutex<()> = Mutex::new(());

unsafe extern "C" fn record_previous_handler(signal: libc::c_int) {
    let pid = FORWARD_PROBE_PID.load(Ordering::SeqCst);
    if pid != 0 {
        // SAFETY: kill with signal zero is an async-signal-safe existence
        // probe for the fixture child, including an unreaped zombie.
        FORWARDED_CHILD_PROBE.store(unsafe { libc::kill(pid, 0) }, Ordering::SeqCst);
    }
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

/// The sandbox spawn seam holds the actual post-spawn, pre-registration
/// window open, without adding a production test hook. ORB-14720: a signal
/// here must not take the previous disposition and orphan the child group.
#[test]
fn signals_in_the_spawn_window_are_pending_until_cleanup_and_reraised() {
    let _lock = TEST_LOCK.lock().expect("signal test lock");
    for signal in [libc::SIGINT, libc::SIGTERM] {
        for streaming in [false, true] {
            for before_spawn in [false, true] {
                FORWARDED_SIGNAL.store(0, Ordering::SeqCst);
                FORWARD_PROBE_PID.store(0, Ordering::SeqCst);
                FORWARDED_CHILD_PROBE.store(0, Ordering::SeqCst);
                install_previous_handler(signal);
                let (ready_tx, ready_rx) = mpsc::sync_channel(1);
                let (resume_tx, resume_rx) = mpsc::sync_channel(1);
                let sandbox = PausedSpawn {
                    before_spawn,
                    ready: ready_tx,
                    resume: Mutex::new(resume_rx),
                    group: Mutex::new(SpawnGroup::default()),
                };
                let req = ExecRequest {
                    program: "/bin/sleep".to_string(),
                    args: vec!["60".to_string()],
                    current_dir: None,
                    timeout_ms: Some(15_000),
                    stdin_mode: StdinMode::Null,
                    environment_mode: EnvironmentMode::Inherit,
                    debug: false,
                };

                let (result, forwarded_while_paused, peer_status) = thread::scope(|scope| {
                    let runner = scope.spawn(|| {
                        if streaming {
                            run_process_streaming_stdout(&req, &sandbox, |mut stdout| {
                                std::io::copy(&mut stdout, &mut std::io::sink())?;
                                Ok(())
                            })
                            .map(|(result, ())| result)
                        } else {
                            run_process(&req, &sandbox)
                        }
                    });
                    ready_rx
                        .recv_timeout(Duration::from_secs(30))
                        .expect("runner reached held spawn window");
                    // SAFETY: signal only this dedicated test process, whose
                    // previous handler records the eventual re-raise.
                    assert_eq!(unsafe { libc::raise(signal) }, 0, "signal runner");
                    let forwarded = FORWARDED_SIGNAL.load(Ordering::SeqCst);
                    resume_tx.send(()).expect("release held spawn window");
                    // Reap our separately waitable peer promptly, so a zombie
                    // cannot keep the group present during supervisor cleanup.
                    let mut peer = sandbox.group.lock().expect("fixture group").peer.take();
                    let peer_status = peer.as_mut().and_then(|peer| {
                        peer.wait_timeout(Duration::from_secs(10))
                            .expect("wait for group peer")
                    });
                    sandbox.group.lock().expect("fixture group").peer = peer;
                    (
                        runner.join().expect("runner thread"),
                        forwarded,
                        peer_status,
                    )
                });

                assert_eq!(
                    forwarded_while_paused, 0,
                    "ORB-14720: a pre-spawn or post-spawn signal must remain pending"
                );
                assert_eq!(
                    FORWARDED_SIGNAL.load(Ordering::SeqCst),
                    signal,
                    "restore and re-raise after spawn failure or child cleanup"
                );
                if before_spawn {
                    assert!(
                        matches!(result, Err(OrbitError::Execution(message)) if message == "injected spawn failure")
                    );
                } else {
                    assert_interrupted(&result.expect("interrupted runner result"), signal);
                    assert_eq!(
                        FORWARDED_CHILD_PROBE.load(Ordering::SeqCst),
                        -1,
                        "ORB-14720: the previous handler must observe an already reaped child"
                    );
                    let mut group = sandbox.group.lock().expect("fixture group");
                    let pid = group.pid.expect("group leader pid");
                    // SAFETY: WNOHANG probes the fixture's direct child only.
                    let waited = unsafe {
                        libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), libc::WNOHANG)
                    };
                    assert_eq!(
                        (waited, std::io::Error::last_os_error().raw_os_error()),
                        (-1, Some(libc::ECHILD)),
                        "ORB-14720: the runner must reap the child before forwarding"
                    );
                    assert_eq!(peer_status.and_then(|status| status.signal()), Some(signal));
                    // SAFETY: signal zero probes this fixture's process group.
                    let probe = unsafe { libc::killpg(pid as libc::pid_t, 0) };
                    assert_eq!(
                        (probe, std::io::Error::last_os_error().raw_os_error()),
                        (-1, Some(libc::ESRCH)),
                        "ORB-14720: no process group may survive the spawn-window interrupt"
                    );
                    group.pid = None;
                }
                FORWARD_PROBE_PID.store(0, Ordering::SeqCst);
                let fresh = supervise_script("exit 0");
                assert!(
                    fresh.success,
                    "the forwarded signal must not leak into a fresh spawn"
                );
            }
        }
    }
}

struct PausedSpawn {
    before_spawn: bool,
    ready: mpsc::SyncSender<()>,
    resume: Mutex<mpsc::Receiver<()>>,
    group: Mutex<SpawnGroup>,
}

impl Sandbox for PausedSpawn {
    fn validate(&self, _req: &ExecRequest) -> Result<(), OrbitError> {
        Ok(())
    }

    fn spawn(&self, req: &ExecRequest) -> Result<Child, OrbitError> {
        if !self.before_spawn {
            let mut group = self.group.lock().expect("fixture group");
            group.child = Some(NoSandbox.spawn(req)?);
            let pid = group.child.as_ref().expect("group leader").id();
            group.pid = Some(pid);
            FORWARD_PROBE_PID.store(pid as i32, Ordering::SeqCst);
            group.peer = Some(
                Command::new("/bin/sleep")
                    .arg("60")
                    .process_group(pid as i32)
                    .spawn()
                    .expect("peer in child process group"),
            );
        }
        self.ready.send(()).expect("announce held spawn window");
        self.resume
            .lock()
            .expect("resume receiver")
            .recv_timeout(Duration::from_secs(30))
            .expect("release spawn window");
        if self.before_spawn {
            return Err(OrbitError::Execution("injected spawn failure".to_string()));
        }
        Ok(self
            .group
            .lock()
            .expect("fixture group")
            .child
            .take()
            .expect("group leader"))
    }
}

#[derive(Default)]
struct SpawnGroup {
    pid: Option<u32>,
    child: Option<Child>,
    peer: Option<Child>,
}

impl Drop for SpawnGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            // SAFETY: only the unreleased fixture group is signalled.
            unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
        }
        for child in self.child.iter_mut().chain(self.peer.iter_mut()) {
            let _ = child.kill();
            let _ = child.wait();
        }
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

#[test]
fn pending_sigterm_interrupts_late_supervisors_without_delaying_forwarding() {
    let _lock = TEST_LOCK.lock().expect("signal test lock");
    FORWARDED_SIGNAL.store(0, Ordering::SeqCst);
    install_previous_handler(libc::SIGTERM);

    let dir = tempfile::tempdir().expect("tempdir");
    let ready = dir.path().join("ignoring-term.ready");
    let script = format!("trap '' TERM; touch {} && exec sleep 30", ready.display());
    // The ignoring child needs the five-second termination grace period.
    let forwarding_bound = Duration::from_secs(7);

    thread::scope(|scope| {
        let first = scope.spawn(|| supervise_script(&script));
        wait_for_marker(&ready);
        wait_for_supervisor_handler(libc::SIGTERM);

        let signalled = Instant::now();
        // Safety: the installed supervisor handler receives SIGTERM in this
        // dedicated test binary, with the recorder as its previous handler.
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0, "raise SIGTERM");
        assert_eq!(FORWARDED_SIGNAL.load(Ordering::SeqCst), 0);

        // Both arrive after SIGTERM, while the ignoring child keeps the
        // handler installed. Observing shutdown must not consume the signal.
        let late_second = scope.spawn(|| supervise_script("exec sleep 30"));
        let late_third = scope.spawn(|| supervise_script("exec sleep 30"));
        let forwarded_in_time = loop {
            if FORWARDED_SIGNAL.load(Ordering::SeqCst) == libc::SIGTERM {
                break true;
            }
            if signalled.elapsed() >= forwarding_bound {
                break false;
            }
            thread::sleep(Duration::from_millis(20));
        };

        // Join before asserting so failures still let the supervisors reap
        // every child; the request timeout bounds the unfixed regression.
        let first = first.join().expect("first supervisor thread");
        let second = late_second.join().expect("second supervisor thread");
        let third = late_third.join().expect("third supervisor thread");
        assert_interrupted(&first, libc::SIGTERM);
        assert_interrupted(&second, libc::SIGTERM);
        assert_interrupted(&third, libc::SIGTERM);
        assert!(
            forwarded_in_time,
            "late supervisors must let the previous SIGTERM handler run within \
             the termination grace period plus margin"
        );
    });

    let fresh = supervise_script("exit 0");
    assert!(
        fresh.success,
        "forwarded SIGTERM must not interrupt a new wait"
    );
    assert_eq!(fresh.exit_code, Some(0));
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
    orbit_common::test_env::assert_child_test_exists(
        "sigterm_with_default_disposition_exits_the_supervisor",
    );
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
    supervise_script(&script)
}

fn supervise_script(script: &str) -> orbit_exec::ExecutionResult {
    let req = ExecRequest {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        current_dir: None,
        timeout_ms: Some(15_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::Inherit,
        debug: false,
    };
    run_process(&req, &NoSandbox).expect("run_process")
}

fn wait_for_supervisor_handler(signal: libc::c_int) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        // Safety: a null new action only queries the current disposition.
        let action = unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            assert_eq!(libc::sigaction(signal, std::ptr::null(), &mut action), 0);
            action
        };
        if action.sa_sigaction != record_previous_handler as *const () as usize {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("supervisor did not install its signal handler");
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
