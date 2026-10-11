//! Cleanup ownership before the confined provider reaches supervision.

use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_common::test_env::FIXTURE_STEP_DEADLINE;
use orbit_engine::PluginBrokerHandle;

use super::{Confined, Node, ResolvedFsProfile, fs, wait_for};

/// Sandbox launchers create a group led by the returned child's PID. Own it
/// before registration, broker binding or any readiness assertion can fail.
pub(super) struct ProviderChild {
    child: Option<Child>,
    watchdog: Option<Child>,
}

impl ProviderChild {
    pub(super) fn new(child: Child) -> Self {
        let mut owned = Self {
            child: Some(child),
            watchdog: None,
        };
        // The outer child-test runner kills the fixture's group on overrun.
        // Sandbox providers lead separate groups, so a pipe watchdog must
        // also cover abrupt fixture death, when Rust destructors cannot run.
        // Only this fixture holds the pipe writer; EOF wakes the watchdog.
        let watchdog = Command::new("/bin/sh")
            .args([
                "-c",
                "while read -r _; do :; done; kill -KILL \"-$1\"",
                "claimed-review-provider-watchdog",
            ])
            .arg(owned.id().to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .expect("watch the confined provider group");
        owned.watchdog = Some(watchdog);
        owned
    }

    pub(super) fn id(&self) -> u32 {
        self.child.as_ref().expect("owned provider").id()
    }

    pub(super) fn supervise(mut self) -> orbit_types::tool::ExecutionResult {
        // The supervisor assumes kill/reap ownership. Keep the watchdog
        // armed until it finishes, including its setup-error cleanup path.
        orbit_exec::supervise_child(
            self.child.take().expect("owned provider"),
            Some(600_000),
            None,
        )
        .expect("supervise sandbox")
        .result
    }
}

impl Drop for ProviderChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Do not try_wait first: an exited but unreaped leader still
            // reserves its PID, and its descendants may still be alive.
            // SAFETY: both sandbox launchers make this owned child its group
            // leader. It has not been handed to or reaped by another waiter.
            unsafe { libc::killpg(child.id() as libc::pid_t, libc::SIGKILL) };
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(mut watchdog) = self.watchdog.take() {
            // Keep stdin open until the watchdog has been reaped, so it
            // cannot signal a group ID already released by supervision.
            let _ = watchdog.kill();
            let _ = watchdog.wait();
        }
    }
}

const OUTER: &str = "claimed_review_bridge_sandbox::cleanup::abrupt_fixture";
const ROOT_ENV: &str = "ORBIT_CLAIMED_REVIEW_CLEANUP_ROOT";
/// The error the fault-injecting broker returns from sandbox authentication.
const INJECTED: &str = "injected broker bind failure";
/// Locks the blocked provider and its descendant hold while they run. Exit
/// closes a process's descriptors and releases its lock, so a free lock
/// proves its holder is gone, through Bubblewrap's PID namespace and new
/// session, which put the descendant in no group of ours, as under Seatbelt.
const HOLDERS: [&str; 2] = ["provider.lock", "descendant.lock"];

// The descendant takes its lock and acknowledges that it is running before
// the provider takes its own and writes readiness. Both then block on a
// signal, with no timing load. The case directory is the last argument, so
// the program runs inline or as the script `Confined::spawn` confines.
const BLOCKED_PROVIDER: &str = r#"import fcntl, os, signal, subprocess, sys
case = sys.argv[-1]
child = subprocess.Popen([sys.executable, '-c',
    'import fcntl, signal, sys\n'
    'held = open(sys.argv[1], "w")\n'
    'fcntl.flock(held, fcntl.LOCK_EX)\n'
    'print("ready", flush=True)\n'
    'signal.pause()\n',
    os.path.join(case, 'descendant.lock')], stdout=subprocess.PIPE)
assert child.stdout.readline() == b'ready\n'
held = open(os.path.join(case, 'provider.lock'), 'w')
fcntl.flock(held, fcntl.LOCK_EX)
with open(os.path.join(case, 'ready'), 'w') as marker:
    marker.write('ready')
signal.pause()
"#;

fn raw_provider(case: &Path) -> ProviderChild {
    ProviderChild::new(
        Command::new("/usr/bin/python3")
            .args(["-c", BLOCKED_PROVIDER])
            .arg(case)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .expect("blocked provider"),
    )
}

/// The provider's stderr, which its descendant inherits.
fn take_stderr(child: &mut ProviderChild) -> std::process::ChildStderr {
    child
        .child
        .as_mut()
        .and_then(|child| child.stderr.take())
        .expect("provider stderr")
}

/// The blocked provider in `case`, once it and its descendant hold their
/// locks, with its PID and its stderr.
fn running_raw(case: &Path) -> (ProviderChild, u32, std::process::ChildStderr) {
    fs::create_dir(case).unwrap();
    let mut child = raw_provider(case);
    let (pid, stderr) = (child.id(), take_stderr(&mut child));
    wait_for(&case.join("ready"), FIXTURE_STEP_DEADLINE);
    assert_holders_running(case);
    (child, pid, stderr)
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("<non-string panic payload>")
}

/// Whether `lock` is free now. A lock taken here is released when the
/// descriptor closes on return.
fn lock_free(lock: &Path) -> bool {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock)
        .unwrap_or_else(|error| panic!("open {}: {error}", lock.display()));
    // SAFETY: `file` is a live descriptor owned here; LOCK_NB never blocks.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return true;
    }
    let error = std::io::Error::last_os_error();
    assert_eq!(
        error.raw_os_error(),
        Some(libc::EWOULDBLOCK),
        "lock {}: {error}",
        lock.display()
    );
    false
}

fn assert_holders_running(case: &Path) {
    for holder in HOLDERS {
        assert!(
            !lock_free(&case.join(holder)),
            "the blocked provider tree holds {holder} before the injected failure"
        );
    }
}

fn assert_holders_gone(case: &Path) {
    for holder in HOLDERS {
        let started = Instant::now();
        while !lock_free(&case.join(holder)) {
            assert!(
                started.elapsed() < FIXTURE_STEP_DEADLINE,
                "a provider process still holds {holder} after cleanup"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn assert_group_gone(pid: u32) {
    let started = Instant::now();
    loop {
        // SAFETY: signal zero only queries membership in this fixture group.
        let result = unsafe { libc::killpg(pid as libc::pid_t, 0) };
        if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return;
        }
        assert!(
            started.elapsed() < FIXTURE_STEP_DEADLINE,
            "provider group {pid} survived cleanup"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn assert_reaped(pid: u32) {
    let mut status = 0;
    // SAFETY: status points to writable storage; WNOHANG never blocks.
    assert_eq!(
        unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD),
        "the provider must already have been reaped by its owner"
    );
}

fn assert_descendant_pipe_closed(mut pipe: std::process::ChildStderr) {
    use std::io::Read;

    // The blocked descendant inherits stderr, so EOF also proves it exited.
    // SAFETY: this is our live read descriptor; nonblocking affects no writer.
    assert_ne!(
        unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        -1
    );
    let started = Instant::now();
    let mut bytes = [0; 1024];
    loop {
        match pipe.read(&mut bytes) {
            Ok(0) => return,
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => panic!("read provider stderr: {error}"),
        }
        assert!(
            started.elapsed() < FIXTURE_STEP_DEADLINE,
            "a provider descendant still holds stderr after cleanup"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Every trace of the provider tree in `case` is gone: the provider was
/// reaped by its owner, its group is empty and no process holds a lock.
fn assert_cleaned_up(case: &Path, pid: u32) {
    assert_reaped(pid);
    assert_group_gone(pid);
    assert_holders_gone(case);
}

/// Fail a readiness wait while `owner` holds the running provider, as the
/// bridge fixture's `wait_for` can before `finish`: the wait has already
/// expired, so the failure is deterministic.
fn fail_readiness<T>(case: &Path, owner: T) {
    let missing = case.join("never-ready");
    let expected = format!("timed out waiting for {}", missing.display());
    let error = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _owned = owner;
        wait_for(&missing, Duration::ZERO);
    }))
    .expect_err("readiness must fail");
    assert_eq!(
        panic_message(&*error),
        expected,
        "cleanup preserves the original readiness panic"
    );
}

#[test]
fn pre_supervision_failures_reap_the_provider_and_end_its_group() {
    let root = tempfile::tempdir().unwrap();

    // An error returned while the guard owns the provider.
    let case = root.path().join("error");
    let (child, pid, stderr) = running_raw(&case);
    let original = "injected worker registration failure";
    let register = |child: ProviderChild| -> Result<(), &str> {
        let _owned = child;
        Err(original)
    };
    assert_eq!(
        register(child),
        Err(original),
        "cleanup preserves the original error"
    );
    assert_cleaned_up(&case, pid);
    assert_descendant_pipe_closed(stderr);

    // A readiness panic unwinding past the guard.
    let case = root.path().join("readiness");
    let (child, pid, stderr) = running_raw(&case);
    fail_readiness(&case, child);
    assert_cleaned_up(&case, pid);
    assert_descendant_pipe_closed(stderr);
}

#[test]
fn ready_provider_transfers_to_supervision_and_returns_its_report() {
    let child = ProviderChild::new(
        Command::new("/usr/bin/python3")
            .args(["-c", "import json; print(json.dumps({'completed': True}))"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    let pid = child.id();
    let output = child.supervise();
    assert!(output.success && !output.timed_out, "{output:?}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&output.stdout).unwrap()["completed"],
        true
    );
    assert_reaped(pid);
    assert_group_gone(pid);
}

/// A broker that records the sandbox it authenticates. With `fail`, it
/// waits until the provider and its descendant run, then refuses: fault
/// injection into the bridge fixture's own `Confined::spawn`, after its
/// sandbox launch and worker registration and before it returns.
struct InjectedBroker {
    socket: PathBuf,
    case: PathBuf,
    fail: bool,
    pid: AtomicU32,
}

impl PluginBrokerHandle for InjectedBroker {
    fn socket_path(&self) -> &Path {
        &self.socket
    }

    fn bind_sandbox(&self, sandbox_pid: u32) -> Result<(), OrbitError> {
        self.pid.store(sandbox_pid, Ordering::SeqCst);
        if !self.fail {
            return Ok(());
        }
        wait_for(&self.case.join("ready"), FIXTURE_STEP_DEADLINE);
        assert_holders_running(&self.case);
        Err(OrbitError::Execution(INJECTED.to_string()))
    }
}

#[test]
fn confined_pre_supervision_failures_reap_the_provider_and_end_its_group() {
    if !orbit_exec::macos_sandbox_test_guard(
        "confined_pre_supervision_failures_reap_the_provider_and_end_its_group",
    ) {
        return;
    }
    #[cfg(target_os = "linux")]
    if let Some(reason) = super::sandbox_unavailable() {
        orbit_exec::report_bwrap_deferral(
            "confined_pre_supervision_failures_reap_the_provider_and_end_its_group",
            &reason,
        );
        return;
    }
    let dir = tempfile::Builder::new()
        .prefix("ocr-cleanup")
        .tempdir_in("/var/tmp")
        .unwrap();
    let root = dir.path().canonicalize().unwrap();
    let follower = Node::init(&root, "follower", "TSC");
    let runner = follower.runtime();
    let provider = root.join("blocked-provider.py");
    fs::write(&provider, BLOCKED_PROVIDER).unwrap();
    let profile = ResolvedFsProfile {
        name: "claimed_review_cleanup".to_string(),
        read: vec!["/**".to_string()],
        modify: vec![format!("{}/**", root.display())],
    };
    let home = [("HOME", follower.home.display().to_string())];
    for failure in ["binding", "readiness"] {
        let case = root.join(failure);
        fs::create_dir(&case).unwrap();
        let broker = InjectedBroker {
            socket: case.join("broker.sock"),
            case: case.clone(),
            fail: failure == "binding",
            pid: AtomicU32::new(0),
        };
        let brokers: [&dyn PluginBrokerHandle; 1] = [&broker];
        let spawn = || {
            Confined::spawn(
                &runner,
                &follower,
                &profile,
                &provider,
                &[case.to_str().unwrap()],
                &brokers,
                &home,
            )
        };
        if broker.fail {
            let Err(error) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(spawn)) else {
                panic!("the injected binding failure must fail the spawn");
            };
            let message = panic_message(&*error);
            assert!(
                message.starts_with("authenticate the sandbox") && message.contains(INJECTED),
                "cleanup preserves the original binding error: {message}"
            );
            assert_cleaned_up(&case, broker.pid.load(Ordering::SeqCst));
        } else {
            let mut confined = spawn();
            let pid = confined.child.id();
            assert_eq!(broker.pid.load(Ordering::SeqCst), pid, "the bound sandbox");
            let stderr = take_stderr(&mut confined.child);
            wait_for(&case.join("ready"), FIXTURE_STEP_DEADLINE);
            assert_holders_running(&case);
            // The whole `Confined`, so the macOS profile outlives cleanup
            // exactly as it does in the bridge fixture.
            fail_readiness(&case, confined);
            assert_cleaned_up(&case, pid);
            assert_descendant_pipe_closed(stderr);
        }
    }
}

#[test]
fn outer_overrun_also_ends_the_separate_provider_group() {
    orbit_common::test_env::assert_child_test_exists(OUTER);
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    command
        .args(["--ignored", "--exact", OUTER, "--nocapture"])
        .env(ROOT_ENV, root.path());
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // The child reports progress until its provider and descendant run,
        // then blocks in silence. The short quiet deadline therefore starts
        // at readiness, whatever the host's startup latency; the ceiling only
        // bounds a hang.
        orbit_common::test_env::run_child_test_within(
            &mut command,
            OUTER,
            root.path(),
            Duration::from_secs(5),
            orbit_common::test_env::CHILD_TEST_CEILING,
        );
    }))
    .expect_err("the blocked outer fixture must overrun");
    let message = panic_message(&*failure);
    assert!(
        message.contains("printed nothing for"),
        "the runner killed the fixture for its silence: {message}"
    );
    let pid: u32 = fs::read_to_string(root.path().join("provider.pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert_group_gone(pid);
    assert_holders_gone(root.path());
}

#[test]
#[ignore = "exact child for the outer overrun cleanup harness"]
fn abrupt_fixture() {
    use std::io::Write;

    let root = std::env::var_os(ROOT_ENV).expect("cleanup fixture root");
    let root = Path::new(&root);
    let child = raw_provider(root);
    let started = Instant::now();
    while !root.join("ready").exists() {
        assert!(
            started.elapsed() < FIXTURE_STEP_DEADLINE,
            "the blocked provider never became ready"
        );
        let _ = writeln!(std::io::stderr(), "waiting for the blocked provider");
        std::thread::sleep(Duration::from_millis(250));
    }
    fs::write(root.join("provider.pid"), child.id().to_string()).unwrap();
    let _ = writeln!(std::io::stderr(), "provider and descendant ready");
    // The outer runner kills this process, exercising the pipe watchdog
    // instead of Drop. Keep the provider's ownership guard on the stack.
    let _owned = child;
    std::thread::sleep(FIXTURE_STEP_DEADLINE);
    panic!("the outer runner did not kill the silent fixture");
}
