#![allow(missing_docs)]

use std::path::Path;

#[cfg(unix)]
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tempfile::tempdir;

#[cfg(target_os = "linux")]
use orbit_exec::{LinuxBwrapMountAuthority, compile_linux_bwrap_argv_with_authority};
#[cfg(target_os = "linux")]
use orbit_types::policy::ResolvedFsProfile;

#[cfg(target_os = "linux")]
use super::super::spawn::{SpawnedChild, spawn_bare};
use super::super::supervisor::{SpawnTraceContext, SpawnWithTimeoutRequest, spawn_with_timeout};
use super::test_support::{
    assert_event, capture_events, capture_events_live, capture_redacted_tracing_output, sh_args,
};

fn spawn_test_request<'a>(
    program: &'a str,
    args: &'a [String],
    cwd: Option<&'a Path>,
    timeout: Duration,
    trace: SpawnTraceContext<'a>,
) -> SpawnWithTimeoutRequest<'a> {
    SpawnWithTimeoutRequest {
        program,
        args,
        stdin_bytes: b"",
        env: &[],
        cwd,
        timeout,
        sandbox: None,
        trace,
        output_capture_limit: None,
        on_spawn: None,
        wait: None,
        live_readers: None,
        spawned_child: None,
        #[cfg(unix)]
        cancel_pair: None,
    }
}

/// Execute the real supervisor with a descriptor-backed spawned-child guard.
/// The plan must remain owned across its wait loop and be released only after
/// supervision and process-tree cleanup return.
#[cfg(target_os = "linux")]
#[test]
fn supervisor_retains_linux_mount_plan_through_wait_and_cleanup() {
    let temp = tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical root");
    let target = root.join("orbit.db");
    std::fs::write(&target, b"sqlite-object").expect("runtime object");
    let authority = Arc::new(std::fs::File::open(&target).expect("authority"));
    let authority_lifetime = Arc::downgrade(&authority);
    let plan = compile_linux_bwrap_argv_with_authority(
        &ResolvedFsProfile {
            name: "test".to_string(),
            read: vec!["/**".to_string()],
            modify: vec![target.display().to_string()],
        },
        "/bin/true",
        &[],
        Some(&root),
        false,
        vec![LinuxBwrapMountAuthority {
            destination: target,
            source: authority,
        }],
    )
    .expect("descriptor plan");
    let SpawnedChild {
        child,
        _profile_temp,
        _linux_mount_plan: _,
    } = spawn_bare("/bin/sh", &sh_args("exit 0"), &[], Some(&root)).expect("spawn");
    let spawned = SpawnedChild {
        child,
        _profile_temp,
        _linux_mount_plan: Some(plan),
    };
    let wait = |child: &mut std::process::Child| {
        assert!(
            authority_lifetime.upgrade().is_some(),
            "supervisor dropped the mount plan before waiting for the child"
        );
        child.try_wait()
    };
    let args = sh_args("exit 0");
    let cwd_label = root.display().to_string();
    let mut request = spawn_test_request(
        "/bin/sh",
        &args,
        Some(&root),
        Duration::from_secs(5),
        SpawnTraceContext {
            provider: "codex",
            job_run_id: "jrun-descriptor-lifetime",
            task_id: Some("descriptor-lifetime"),
            cwd: Some(&cwd_label),
        },
    );
    request.spawned_child = Some(spawned);
    request.wait = Some(&wait);

    let (_, _, exit_code, _, timed_out) = spawn_with_timeout(request).expect("supervision");
    assert_eq!(exit_code, Some(0));
    assert!(!timed_out);
    assert!(
        authority_lifetime.upgrade().is_none(),
        "supervisor must release the mount plan after cleanup"
    );
}

#[test]
fn spawn_with_timeout_emits_structured_stdout_and_stderr_events() {
    let args = sh_args("printf '%s\\n' out-one out-two; printf '%s\\n' err-one >&2");
    let (result, events) = capture_events(|| {
        spawn_with_timeout(spawn_test_request(
            "/bin/sh",
            &args,
            None,
            Duration::from_secs(5),
            SpawnTraceContext {
                provider: "codex",
                job_run_id: "job-123",
                task_id: Some("T123"),
                cwd: None,
            },
        ))
    });
    let (stdout, stderr, exit_code, _duration, timed_out) = result.expect("spawn succeeds");

    assert_eq!(stdout.bytes(), b"out-one\nout-two\n");
    assert_eq!(stderr.bytes(), b"err-one\n");
    assert_eq!(exit_code, Some(0));
    assert!(!timed_out);
    assert_eq!(events.len(), 3);

    assert_event(&events, "stdout", "out-one");
    assert_event(&events, "stdout", "out-two");
    assert_event(&events, "stderr", "err-one");
    for event in &events {
        assert_eq!(event.field("provider"), Some("codex"));
        assert_eq!(event.field("job_run_id"), Some("job-123"));
        assert_eq!(event.field("task_id"), Some("T123"));
        assert!(event.fields.contains_key("stream"));
        assert!(event.fields.contains_key("line"));
        assert!(!event.fields.contains_key("cwd"));
    }

    let cwd = tempdir().expect("cwd tempdir");
    let cwd_path = cwd.path().canonicalize().expect("canonical cwd");
    let cwd_string = cwd_path.display().to_string();
    let (result, events) = capture_events(|| {
        spawn_with_timeout(spawn_test_request(
            "/bin/sh",
            &args,
            Some(&cwd_path),
            Duration::from_secs(5),
            SpawnTraceContext {
                provider: "codex",
                job_run_id: "job-456",
                task_id: Some("T456"),
                cwd: Some(cwd_string.as_str()),
            },
        ))
    });
    let (stdout, stderr, exit_code, _duration, timed_out) = result.expect("spawn succeeds");

    assert_eq!(stdout.bytes(), b"out-one\nout-two\n");
    assert_eq!(stderr.bytes(), b"err-one\n");
    assert_eq!(exit_code, Some(0));
    assert!(!timed_out);
    assert_eq!(events.len(), 3);
    for event in &events {
        assert_eq!(event.field("cwd"), Some(cwd_string.as_str()));
    }
}

#[cfg(unix)]
#[test]
fn spawn_with_timeout_reports_the_child_pid_while_the_child_is_still_running() {
    use std::sync::Mutex;

    // The child holds the PID observable long enough for the callback's view of
    // it to be checked against a live process, which is what the run-status
    // surface actually needs: a PID reported mid-invocation, not post-mortem.
    let args = sh_args("printf '%s\\n' started; sleep 0.3");
    let observed: Mutex<Vec<u32>> = Mutex::new(Vec::new());
    let alive_at_callback = Mutex::new(None);
    let on_spawn = |pid: u32| {
        observed.lock().expect("observed lock").push(pid);
        *alive_at_callback.lock().expect("alive lock") = Some(process_is_live(pid));
    };

    let mut request = spawn_test_request(
        "/bin/sh",
        &args,
        None,
        Duration::from_secs(5),
        SpawnTraceContext {
            provider: "codex",
            job_run_id: "job-pid",
            task_id: Some("TPID"),
            cwd: None,
        },
    );
    request.on_spawn = Some(&on_spawn);

    let (stdout, _stderr, exit_code, _duration, timed_out) =
        spawn_with_timeout(request).expect("spawn succeeds");

    assert_eq!(exit_code, Some(0));
    assert!(!timed_out);
    assert_eq!(stdout.bytes(), b"started\n");

    let observed = observed.into_inner().expect("observed pids");
    assert_eq!(observed.len(), 1, "the pid is reported exactly once");
    assert_ne!(observed[0], 0);
    assert_ne!(
        observed[0],
        std::process::id(),
        "the reported pid must be the child, not the supervisor"
    );
    assert_eq!(
        alive_at_callback.into_inner().expect("alive flag"),
        Some(true),
        "the pid must be reported while the child is still running"
    );
}

#[cfg(unix)]
#[test]
fn spawn_with_timeout_cleans_process_group_when_wait_fails() {
    use std::cell::Cell;
    use std::io;

    let args = sh_args("sleep 30");
    let child_pid = Cell::new(0u32);
    let on_spawn = |pid: u32| child_pid.set(pid);
    let wait = |_child: &mut std::process::Child| Err(io::Error::other("injected wait failure"));
    let mut request = spawn_test_request(
        "/bin/sh",
        &args,
        None,
        Duration::from_secs(5),
        SpawnTraceContext {
            provider: "codex",
            job_run_id: "job-wait-error",
            task_id: Some("TWAIT"),
            cwd: None,
        },
    );
    let live_readers = Arc::new(AtomicUsize::new(0));
    request.on_spawn = Some(&on_spawn);
    request.wait = Some(&wait);
    request.live_readers = Some(Arc::clone(&live_readers));

    let error = spawn_with_timeout(request).expect_err("injected wait failure");
    assert!(!error.permanent);
    assert!(error.message.contains("injected wait failure"));
    assert_eq!(
        live_readers.load(Ordering::SeqCst),
        0,
        "wait-error must join output readers before returning"
    );

    let pid = child_pid.get();
    assert_ne!(pid, 0);
    let result = unsafe { libc::killpg(pid as libc::pid_t, 0) };
    assert_eq!(result, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}

#[test]
fn spawn_with_timeout_redacts_tracing_line_without_redacting_raw_stdout() {
    let args = sh_args("printf '%s\\n' 'Authorization: Bearer abc123'");
    let (result, formatted_output) = capture_redacted_tracing_output(|| {
        spawn_with_timeout(spawn_test_request(
            "/bin/sh",
            &args,
            None,
            Duration::from_secs(5),
            SpawnTraceContext {
                provider: "codex",
                job_run_id: "job-redact",
                task_id: Some("TRED"),
                cwd: None,
            },
        ))
    });
    let (stdout, stderr, exit_code, _duration, timed_out) = result.expect("spawn succeeds");

    assert_eq!(stdout.bytes(), b"Authorization: Bearer abc123\n");
    assert!(stderr.bytes().is_empty());
    assert_eq!(exit_code, Some(0));
    assert!(!timed_out);
    assert!(formatted_output.contains("[REDACTED_AUTH]"));
    assert!(
        !formatted_output.contains("abc123"),
        "formatted tracing output leaked secret: {formatted_output}"
    );
}

#[cfg(unix)]
#[test]
fn spawn_with_timeout_captures_delayed_stdout_when_cancel_pair_creation_fails() {
    use std::io;

    let args = sh_args("sleep 0.1; printf '%s\\n' delayed-output");
    let cancel_pair = || Err(io::Error::other("injected cancel pair failure"));
    let mut request = spawn_test_request(
        "/bin/sh",
        &args,
        None,
        Duration::from_secs(5),
        SpawnTraceContext {
            provider: "codex",
            job_run_id: "job-cancel-pair-failure",
            task_id: Some("TCANCELPAIR"),
            cwd: None,
        },
    );
    request.cancel_pair = Some(&cancel_pair);

    let (stdout, stderr, exit_code, _duration, timed_out) =
        spawn_with_timeout(request).expect("spawn succeeds without a cancel pair");

    assert_eq!(stdout.bytes(), b"delayed-output\n");
    assert!(stderr.bytes().is_empty());
    assert_eq!(exit_code, Some(0));
    assert!(!timed_out);
}

#[test]
fn spawn_with_timeout_kills_timed_out_process_and_keeps_partial_output() {
    let args = sh_args("printf '%s\\n' 'before timeout'; sleep 1; printf '%s\\n' after");
    let live_readers = Arc::new(AtomicUsize::new(0));
    let (result, events) = capture_events(|| {
        let mut request = spawn_test_request(
            "/bin/sh",
            &args,
            None,
            Duration::from_millis(75),
            SpawnTraceContext {
                provider: "codex",
                job_run_id: "job-timeout",
                task_id: Some("TTIME"),
                cwd: None,
            },
        );
        request.live_readers = Some(Arc::clone(&live_readers));
        spawn_with_timeout(request)
    });
    let (stdout, stderr, exit_code, _duration, timed_out) = result.expect("spawn succeeds");

    assert_eq!(stdout.bytes(), b"before timeout\n");
    assert!(stderr.bytes().is_empty());
    assert_eq!(exit_code, None);
    assert!(timed_out);
    assert_eq!(
        live_readers.load(Ordering::SeqCst),
        0,
        "timeout must join output readers before returning"
    );
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].field("stream"), Some("stdout"));
    assert_eq!(events[0].field("line"), Some("before timeout"));
}

#[test]
fn spawn_with_timeout_bounds_verbose_output_without_killing_the_process() {
    let args = sh_args(
        "i=0; while [ $i -lt 200 ]; do printf 'event-%04d verbose-output-line\\n' \"$i\"; i=$((i + 1)); done; printf '%s\\n' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}'",
    );
    let mut request = spawn_test_request(
        "/bin/sh",
        &args,
        None,
        Duration::from_secs(5),
        SpawnTraceContext {
            provider: "codex",
            job_run_id: "job-output-cap",
            task_id: Some("TCAP"),
            cwd: None,
        },
    );
    request.output_capture_limit = Some(256);

    let started = std::time::Instant::now();
    let (stdout, stderr, exit_code, _duration, timed_out) =
        spawn_with_timeout(request).expect("spawn succeeds");

    assert!(!timed_out);
    assert_eq!(exit_code, Some(0));
    assert!(stderr.bytes().is_empty());
    assert!(stdout.truncated());
    assert_eq!(stdout.capture_limit_bytes(), 256);
    assert!(stdout.observed_bytes() > stdout.capture_limit_bytes());
    assert!(stdout.bytes().len() < stdout.observed_bytes());
    assert!(
        String::from_utf8_lossy(stdout.protocol_bytes()).contains("\"status\":\"success\""),
        "the retained protocol tail must include the final response envelope"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "bounded capture should drain finite verbose output promptly"
    );
}

#[cfg(unix)]
#[test]
fn spawn_with_timeout_kills_grandchild_holding_output_pipes() {
    let pid_dir = tempdir().expect("pid tempdir");
    let pid_file = pid_dir.path().join("grandchild.pid");
    let script = format!(
        "(sleep 30) & child=$!; printf '%s\\n' \"$child\" > {}; printf '%s\\n' 'before timeout'; sleep 30",
        shell_quote(pid_file.to_string_lossy().as_ref())
    );
    let args = sh_args(&script);

    let started = std::time::Instant::now();
    let (stdout, stderr, exit_code, duration, timed_out) = spawn_with_timeout(spawn_test_request(
        "/bin/sh",
        &args,
        None,
        Duration::from_millis(150),
        SpawnTraceContext {
            provider: "codex",
            job_run_id: "job-timeout-tree",
            task_id: Some("TTREE"),
            cwd: None,
        },
    ))
    .expect("spawn succeeds");

    assert!(timed_out);
    assert_eq!(exit_code, None);
    assert_eq!(stdout.bytes(), b"before timeout\n");
    assert!(stderr.bytes().is_empty());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "timeout path should return promptly; reported duration={duration:?}"
    );

    let grandchild_pid = read_pid(&pid_file);
    assert!(
        wait_until(Duration::from_secs(2), || !process_is_live(grandchild_pid)),
        "grandchild process {grandchild_pid} should be gone after timeout"
    );
}

/// A helper the agent detached into its own session outlives the child and
/// the group kill, and keeps the stdout pipe open. The child exited normally,
/// so the run must still finish promptly with the output it produced, join
/// its readers, and ignore later helper writes.
///
/// Cancellation uses Unix `poll` + a wakeup socketpair. Non-Unix platforms
/// keep blocking reads and are not tested here.
#[cfg(unix)]
#[test]
fn spawn_with_timeout_returns_after_a_normal_exit_despite_an_escaped_pipe_holder() {
    if std::process::Command::new("perl")
        .arg("-e")
        .arg("1")
        .output()
        .is_err()
    {
        return;
    }
    let pid_dir = tempdir().expect("pid tempdir");
    let pid_file = pid_dir.path().join("escaped.pid");
    let ready_file = pid_dir.path().join("escaped.ready");
    let write_now_file = pid_dir.path().join("escaped.write-now");
    let wrote_file = pid_dir.path().join("escaped.wrote");
    let escaped_helper = EscapedProcessGuard::new(pid_file.clone());
    // The delayed helper establishes its detached session and records its PID
    // before telling the parent it may exit. That makes the parent wait for a
    // verified escaped pipe holder instead of racing the supervisor's group
    // cleanup against helper startup. After the supervisor returns, the test
    // signals the helper to write again so post-return emission can be checked.
    let script = format!(
        "perl -MPOSIX -e '$SIG{{PIPE}}=\"IGNORE\"; $| = 1; select(undef, undef, undef, 0.2); POSIX::setsid(); open(my $pid, \">\", $ARGV[0]); print $pid $$; close $pid; open(my $ready, \">\", $ARGV[1]); print $ready \"ready\"; close $ready; while (!-s $ARGV[2]) {{ select(undef, undef, undef, 0.05); }} print STDOUT \"after-return\\n\"; open(my $wrote, \">\", $ARGV[3]); print $wrote \"wrote\"; close $wrote; sleep 30' {pid} {ready} {write_now} {wrote} & while [ ! -s {ready} ]; do sleep 0.01; done; printf '%s\n' 'done'",
        pid = shell_quote(pid_file.to_string_lossy().as_ref()),
        ready = shell_quote(ready_file.to_string_lossy().as_ref()),
        write_now = shell_quote(write_now_file.to_string_lossy().as_ref()),
        wrote = shell_quote(wrote_file.to_string_lossy().as_ref()),
    );
    let args = sh_args(&script);
    let live_readers = Arc::new(AtomicUsize::new(0));

    let started = std::time::Instant::now();
    let (result, log) = capture_events_live(|| {
        let mut request = spawn_test_request(
            "/bin/sh",
            &args,
            None,
            Duration::from_secs(20),
            SpawnTraceContext {
                provider: "codex",
                job_run_id: "job-escaped-holder",
                task_id: Some("TESC"),
                cwd: None,
            },
        );
        request.live_readers = Some(Arc::clone(&live_readers));
        spawn_with_timeout(request)
    });
    let (stdout, _stderr, exit_code, _duration, timed_out) = result.expect("spawn succeeds");

    let escaped_pid = read_pid(&pid_file);
    assert!(
        process_is_live(escaped_pid),
        "the escaped helper must remain live after the parent exits"
    );
    assert_eq!(
        live_readers.load(Ordering::SeqCst),
        0,
        "escaped-pipe readers must join before the supervisor returns"
    );

    assert!(!timed_out);
    assert_eq!(exit_code, Some(0));
    assert_eq!(stdout.bytes(), b"done\n");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a normal exit must not wait on the escaped pipe holder"
    );

    let events = log.snapshot();
    assert_event(&events, "stdout", "done");
    assert!(
        events
            .iter()
            .all(|event| event.field("line") != Some("after-return")),
        "pre-return events must not include helper output written later; events={events:?}"
    );

    std::fs::write(&write_now_file, "now").expect("signal helper to write");
    assert!(
        wait_until(Duration::from_secs(2), || wrote_file.exists()),
        "escaped helper should write after the supervisor returns"
    );
    let events_after = log.snapshot();
    assert!(
        events_after
            .iter()
            .all(|event| event.field("line") != Some("after-return")),
        "escaped helper output after return must not be emitted; events={events_after:?}"
    );
    assert_eq!(events_after.len(), events.len());

    drop(escaped_helper);
    assert!(
        wait_until(Duration::from_secs(2), || !process_is_live(escaped_pid)),
        "escaped helper {escaped_pid} should be gone after test cleanup"
    );
}

#[cfg(unix)]
struct EscapedProcessGuard {
    pid_file: PathBuf,
}

#[cfg(unix)]
impl EscapedProcessGuard {
    fn new(pid_file: PathBuf) -> Self {
        Self { pid_file }
    }
}

#[cfg(unix)]
impl Drop for EscapedProcessGuard {
    fn drop(&mut self) {
        let Ok(contents) = std::fs::read_to_string(&self.pid_file) else {
            return;
        };
        let Ok(pid) = contents.trim().parse::<u32>() else {
            return;
        };
        if pid > 0 && pid <= i32::MAX as u32 {
            // SAFETY: the PID comes from the test helper, which we own.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
            let _ = wait_until(Duration::from_secs(2), || !process_is_live(pid));
        }
    }
}

#[cfg(unix)]
fn read_pid(path: &Path) -> u32 {
    std::fs::read_to_string(path)
        .expect("read pid file")
        .trim()
        .parse()
        .expect("parse pid")
}

#[cfg(unix)]
fn wait_until<F>(timeout: Duration, mut condition: F) -> bool
where
    F: FnMut() -> bool,
{
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    condition()
}

#[cfg(unix)]
fn process_is_live(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        return false;
    }
    let output = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output();
    let Ok(output) = output else {
        return true;
    };
    if !output.status.success() {
        return false;
    }
    let status = String::from_utf8_lossy(&output.stdout);
    !status.trim_start().starts_with('Z')
}

#[cfg(unix)]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
