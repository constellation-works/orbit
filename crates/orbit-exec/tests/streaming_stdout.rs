#![allow(missing_docs)]
#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! Public streaming-runner behavior. Relay allocation failures run in an
//! isolated process because the fixture changes its own descriptor limit and
//! exhausts its descriptor table after a real spawn.

use std::cell::RefCell;
use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_exec::{
    EnvironmentMode, ExecRequest, NoSandbox, Sandbox, StdinMode, run_process_streaming_stdout,
};
use wait_timeout::ChildExt;

fn request(script: &str) -> ExecRequest {
    ExecRequest {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        current_dir: None,
        timeout_ms: Some(5_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::Inherit,
        debug: false,
    }
}

#[derive(Clone, Copy, Debug)]
enum SetupFailure {
    MissingStdout,
    DescriptorExhaustion,
}

#[derive(Default)]
struct ProcessGroup {
    pid: Option<u32>,
    peer: Option<Child>,
    descriptors: Vec<OwnedFd>,
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            // SAFETY: these are the group and direct child created by this
            // fixture. Reap the child if the runner failed to do so.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
                libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), 0);
            }
        }
        if let Some(peer) = self.peer.as_mut() {
            let _ = peer.kill();
            let _ = peer.wait();
        }
    }
}

struct FailingSetup {
    failure: SetupFailure,
    group: RefCell<ProcessGroup>,
}

impl Sandbox for FailingSetup {
    fn validate(&self, req: &ExecRequest) -> Result<(), OrbitError> {
        NoSandbox.validate(req)
    }

    fn spawn(&self, req: &ExecRequest) -> Result<Child, OrbitError> {
        let mut child = NoSandbox.spawn(req)?;
        let mut group = self.group.borrow_mut();
        group.pid = Some(child.id());
        // A separately waitable member proves group termination without
        // depending on the host init process to reap an orphaned grandchild.
        group.peer = Some(
            Command::new("/bin/sleep")
                .arg("60")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(child.id() as i32)
                .spawn()
                .expect("spawn peer in streaming child's group"),
        );
        match self.failure {
            SetupFailure::MissingStdout => drop(child.stdout.take()),
            SetupFailure::DescriptorExhaustion => loop {
                // SAFETY: duplicate this fixture process's open stderr;
                // OwnedFd closes every successful duplicate after the call.
                let fd = unsafe { libc::dup(libc::STDERR_FILENO) };
                if fd < 0 {
                    assert_eq!(
                        std::io::Error::last_os_error().raw_os_error(),
                        Some(libc::EMFILE),
                        "fixture must exhaust only its own descriptor table"
                    );
                    break;
                }
                // SAFETY: dup returned a new descriptor owned by the fixture.
                group.descriptors.push(unsafe { OwnedFd::from_raw_fd(fd) });
            },
        }
        Ok(child)
    }
}

struct DescriptorLimit(libc::rlimit);

impl DescriptorLimit {
    fn lower() -> Self {
        // SAFETY: getrlimit/setrlimit access this isolated process's limit
        // through an initialized struct; the hard limit is preserved.
        unsafe {
            let mut original: libc::rlimit = std::mem::zeroed();
            assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut original), 0);
            let lowered = libc::rlimit {
                rlim_cur: original.rlim_cur.min(128),
                rlim_max: original.rlim_max,
            };
            assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &lowered), 0);
            Self(original)
        }
    }
}

impl Drop for DescriptorLimit {
    fn drop(&mut self) {
        // SAFETY: restore only the soft limit this process lowered.
        unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &self.0) };
    }
}

#[test]
fn relay_setup_errors_kill_the_group_and_reap_the_child() {
    const CHILD_ENV: &str = "ORBIT_EXEC_RELAY_FAILURE_CHILD";
    const CHILD_TEST: &str = "relay_setup_errors_kill_the_group_and_reap_the_child";
    if std::env::var_os(CHILD_ENV).is_none() {
        let mut command = Command::new(std::env::current_exe().expect("test binary"));
        command
            .env(CHILD_ENV, "1")
            .args(["--exact", CHILD_TEST, "--nocapture"]);
        let output = orbit_common::process::run_bounded_capped(
            &mut command,
            Duration::from_secs(15),
            64 * 1024,
        )
        .expect("run isolated descriptor-exhaustion fixture");
        orbit_common::test_env::assert_child_test_passed(
            CHILD_TEST,
            output.status,
            &output.stdout,
            &output.stderr,
        );
        return;
    }

    let _limit = DescriptorLimit::lower();
    for failure in [
        SetupFailure::DescriptorExhaustion,
        SetupFailure::MissingStdout,
    ] {
        let sandbox = FailingSetup {
            failure,
            group: RefCell::new(ProcessGroup::default()),
        };
        let consumed = Arc::new(AtomicBool::new(false));
        let consumer_called = Arc::clone(&consumed);
        let result = run_process_streaming_stdout(&request("exec sleep 60"), &sandbox, move |_| {
            consumer_called.store(true, Ordering::SeqCst);
            Ok(())
        });
        let mut group = sandbox.group.borrow_mut();
        group.descriptors.clear();
        let pid = group.pid.expect("spawned child pid");
        // SAFETY: WNOHANG probes only our direct child. ECHILD proves the
        // runner reaped it, unlike a live child (0) or a zombie (pid).
        let waited =
            unsafe { libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), libc::WNOHANG) };
        let wait_error = std::io::Error::last_os_error().raw_os_error();
        // SAFETY: signal zero only checks whether this fixture child exists.
        let child_probe = unsafe { libc::kill(pid as libc::pid_t, 0) };
        let child_error = std::io::Error::last_os_error().raw_os_error();
        let peer_status = group
            .peer
            .as_mut()
            .expect("group peer")
            .wait_timeout(Duration::from_secs(3))
            .expect("wait for group peer");
        // SAFETY: signal zero probes only the fixture's process group, after
        // reaping its peer so a peer zombie cannot keep the group present.
        let group_probe = unsafe { libc::killpg(pid as libc::pid_t, 0) };
        let group_error = std::io::Error::last_os_error().raw_os_error();
        if group_probe == -1 && group_error == Some(libc::ESRCH) {
            group.pid = None;
        }

        let OrbitError::Execution(message) = result.expect_err("relay setup must fail") else {
            panic!("relay setup must preserve the execution error");
        };
        match failure {
            SetupFailure::DescriptorExhaustion => {
                assert!(message.starts_with("failed to create stdout relay:"));
                assert!(
                    message.contains(&std::io::Error::from_raw_os_error(libc::EMFILE).to_string())
                );
            }
            SetupFailure::MissingStdout => assert_eq!(message, "process stdout was not piped"),
        }
        assert!(
            !consumed.load(Ordering::SeqCst),
            "setup errors must precede consumption"
        );
        assert_eq!(
            (waited, wait_error),
            (-1, Some(libc::ECHILD)),
            "runner must reap its child: {failure:?}"
        );
        assert_eq!(
            (child_probe, child_error),
            (-1, Some(libc::ESRCH)),
            "runner must kill its child: {failure:?}"
        );
        assert_eq!(
            peer_status.and_then(|status| status.signal()),
            Some(libc::SIGKILL),
            "runner must kill the entire process group: {failure:?}"
        );
        assert_eq!(
            (group_probe, group_error),
            (-1, Some(libc::ESRCH)),
            "no process group may survive a relay setup error: {failure:?}"
        );
    }
}

#[test]
fn streaming_preserves_output_timeout_and_consumer_errors() {
    let mut req = request("cat; printf diagnostic >&2");
    let payload = vec![b'x'; 256 * 1024];
    req.stdin_mode = StdinMode::Bytes(payload.clone());
    let (result, bytes) = run_process_streaming_stdout(&req, &NoSandbox, |mut stdout| {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
    .expect("stream successful process");
    assert!(result.success);
    assert!(!result.timed_out);
    assert_eq!(result.exit_code, Some(0));
    assert!(result.stdout.is_empty());
    assert_eq!(result.stderr, "diagnostic");
    assert_eq!(bytes, payload);

    let mut req = request("printf partial; exec sleep 60");
    req.timeout_ms = Some(250);
    let (result, bytes) = run_process_streaming_stdout(&req, &NoSandbox, |mut stdout| {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
    .expect("stream timed-out process");
    assert!(!result.success);
    assert!(result.timed_out);
    assert_eq!(result.exit_code, None);
    assert_eq!(bytes, b"partial");

    let result =
        run_process_streaming_stdout(&request("printf rejected"), &NoSandbox, |mut stdout| {
            let mut bytes = Vec::new();
            stdout.read_to_end(&mut bytes)?;
            assert_eq!(bytes, b"rejected");
            Err::<(), _>(OrbitError::Execution(
                "consumer rejected stream".to_string(),
            ))
        });
    assert!(
        matches!(result, Err(OrbitError::Execution(message)) if message == "consumer rejected stream")
    );
}
