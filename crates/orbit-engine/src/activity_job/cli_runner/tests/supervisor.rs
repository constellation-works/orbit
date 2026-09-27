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
        None,
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
        "perl -MPOSIX -e '$SIG{{PIPE}}=\"IGNORE\"; $| = 1; select(undef, undef, undef, 0.2); POSIX::setsid(); open(my $pid, \">\", $ARGV[0]); print $pid $$; close $pid; open(my $ready, \">\", $ARGV[1]); print $ready \"ready\"; close $ready; while (!-s $ARGV[2]) {{ select(undef, undef, undef, 0.05); }} print STDOUT \"after-return\\n\"; open(my $wrote, \">\", $ARGV[3]); print $wrote \"wrote\"; close $wrote; sleep 30' {pid} {ready} {write_now} {wrote} & perl -e 'my $deadline = time + 5; while (!-s $ARGV[0]) {{ die \"helper did not become ready\\n\" if time >= $deadline; select(undef, undef, undef, 0.05); }}' {ready} && printf '%s\n' 'done'",
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

/// How the supervised parent ends after its escaped helper is ready.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum EscapedHelperExit {
    Normal,
    Timeout,
    WaitError,
}

/// A `/bin/sh` parent that starts a helper in its own session (`setsid`), so
/// the helper survives the process-group kill while holding the parent's
/// stdout/stderr pipes. The helper records its PID and readiness before the
/// parent may end, and every helper is killed when the fixture drops.
#[cfg(unix)]
struct EscapedHelperFixture {
    args: Vec<String>,
    pid_file: PathBuf,
    ready_file: PathBuf,
    _guard: EscapedProcessGuard,
    _dir: tempfile::TempDir,
}

#[cfg(unix)]
impl EscapedHelperFixture {
    /// Returns `None` when `perl` is unavailable.
    fn new(helper_body: &str, exit: EscapedHelperExit) -> Option<Self> {
        if std::process::Command::new("perl")
            .arg("-e")
            .arg("1")
            .output()
            .is_err()
        {
            return None;
        }
        let dir = tempdir().expect("helper tempdir");
        let pid_file = dir.path().join("escaped.pid");
        let ready_file = dir.path().join("escaped.ready");
        let guard = EscapedProcessGuard::new(pid_file.clone());
        let parent_tail = match exit {
            EscapedHelperExit::Normal => "",
            EscapedHelperExit::Timeout | EscapedHelperExit::WaitError => "; sleep 30",
        };
        let script = format!(
            "perl -MPOSIX -e '$SIG{{PIPE}}=\"IGNORE\"; $| = 1; POSIX::setsid() or die \"setsid: $!\\n\"; open(my $pid, \">\", $ARGV[0]); print $pid $$; close $pid; open(my $ready, \">\", $ARGV[1]); print $ready \"ready\"; close $ready; {helper_body}' {pid} {ready} & perl -e 'my $deadline = time + 5; while (!-s $ARGV[0]) {{ die \"helper did not become ready\\n\" if time >= $deadline; select(undef, undef, undef, 0.02); }}' {ready} && printf '%s\\n' done{parent_tail}",
            pid = shell_quote(pid_file.to_string_lossy().as_ref()),
            ready = shell_quote(ready_file.to_string_lossy().as_ref()),
        );
        Some(Self {
            args: sh_args(&script),
            pid_file,
            ready_file,
            _guard: guard,
            _dir: dir,
        })
    }

    fn wait_until_ready(&self) -> bool {
        wait_until(Duration::from_secs(5), || {
            std::fs::metadata(&self.ready_file).is_ok_and(|meta| meta.len() > 0)
        })
    }

    fn helper_pid(&self) -> u32 {
        read_pid(&self.pid_file)
    }
}

/// Helper body that only holds the inherited pipes.
#[cfg(unix)]
const QUIET_HELPER_BODY: &str = "sleep 30;";

/// Helper body that writes as fast as the pipe accepts for up to 20 seconds,
/// then keeps the pipes open. An unbounded drain would run until the loop
/// ends, which the tests' 5-second bound catches without hanging.
#[cfg(unix)]
const BUSY_HELPER_BODY: &str = "my $line = (\"x\" x 4095) . \"\\n\"; my $block = $line x 16; my $end = time + 20; while (time < $end) { print STDOUT $block or last; } sleep 30;";

/// Counts output line events and slows each one so a busy writer always
/// keeps the pipe readable while the reader is emitting.
#[cfg(unix)]
struct SlowCountingSubscriber {
    events: Arc<AtomicUsize>,
}

#[cfg(unix)]
impl tracing::Subscriber for SlowCountingSubscriber {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, _event: &tracing::Event<'_>) {
        self.events.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_micros(200));
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

/// When the pollable cancel channel cannot be created, the reader must not
/// fall back to a blocking `read` that an escaped pipe holder keeps open:
/// supervision still returns within its bound with every reader joined.
#[cfg(unix)]
#[test]
fn spawn_with_timeout_bounds_reader_finalization_when_cancel_pair_creation_fails() {
    use std::cell::Cell;
    use std::io;

    let Some(fixture) = EscapedHelperFixture::new(QUIET_HELPER_BODY, EscapedHelperExit::Normal)
    else {
        return;
    };
    let pair_attempts = Cell::new(0usize);
    let cancel_pair = || {
        pair_attempts.set(pair_attempts.get() + 1);
        Err(io::Error::other("injected cancel pair failure"))
    };
    let live_readers = Arc::new(AtomicUsize::new(0));
    let mut request = spawn_test_request(
        "/bin/sh",
        &fixture.args,
        None,
        Duration::from_secs(20),
        SpawnTraceContext {
            provider: "codex",
            job_run_id: "job-cancel-pair-escaped",
            task_id: Some("TPAIRESC"),
            cwd: None,
        },
    );
    request.cancel_pair = Some(&cancel_pair);
    request.live_readers = Some(Arc::clone(&live_readers));

    let started = std::time::Instant::now();
    let (stdout, _stderr, exit_code, _duration, timed_out) =
        spawn_with_timeout(request).expect("spawn succeeds without a cancel pair");
    let elapsed = started.elapsed();

    assert_eq!(
        pair_attempts.get(),
        2,
        "both readers must take the no-pair path"
    );
    assert!(
        process_is_live(fixture.helper_pid()),
        "the helper must still hold the pipes when the supervisor returns"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "a failed cancel pair must not leave an unbounded reader join; elapsed={elapsed:?}"
    );
    assert_eq!(
        live_readers.load(Ordering::SeqCst),
        0,
        "no output reader may outlive the supervisor"
    );
    assert!(!timed_out);
    assert_eq!(exit_code, Some(0));
    assert_eq!(stdout.bytes(), b"done\n");
    drop(fixture);
}

/// A continuously producing escaped writer must not extend the post-cancel
/// drain. Every terminal path, with and without a cancel pair, returns within
/// the bound with its readers joined, and no line event is emitted after
/// return.
#[cfg(unix)]
#[test]
fn spawn_with_timeout_bounds_post_cancel_drain_against_a_busy_escaped_writer() {
    use std::io;

    let cases = [
        (EscapedHelperExit::Normal, true),
        (EscapedHelperExit::Timeout, true),
        (EscapedHelperExit::WaitError, true),
        (EscapedHelperExit::Normal, false),
    ];
    for (exit, with_cancel_pair) in cases {
        let Some(fixture) = EscapedHelperFixture::new(BUSY_HELPER_BODY, exit) else {
            return;
        };
        let wait_timeout_hook = |_child: &mut std::process::Child| {
            assert!(fixture.wait_until_ready(), "helper did not become ready");
            Ok(None)
        };
        let wait_error_hook = |_child: &mut std::process::Child| {
            assert!(fixture.wait_until_ready(), "helper did not become ready");
            Err(io::Error::other("injected wait failure"))
        };
        let failing_pair = || Err(io::Error::other("injected cancel pair failure"));
        let live_readers = Arc::new(AtomicUsize::new(0));
        let events = Arc::new(AtomicUsize::new(0));
        let dispatch = tracing::Dispatch::new(SlowCountingSubscriber {
            events: Arc::clone(&events),
        });

        let mut request = spawn_test_request(
            "/bin/sh",
            &fixture.args,
            None,
            match exit {
                EscapedHelperExit::Timeout => Duration::from_millis(50),
                EscapedHelperExit::Normal | EscapedHelperExit::WaitError => Duration::from_secs(20),
            },
            SpawnTraceContext {
                provider: "codex",
                job_run_id: "job-busy-escaped-writer",
                task_id: Some("TBUSY"),
                cwd: None,
            },
        );
        request.output_capture_limit = Some(64 * 1024);
        request.live_readers = Some(Arc::clone(&live_readers));
        match exit {
            EscapedHelperExit::Normal => {}
            EscapedHelperExit::Timeout => request.wait = Some(&wait_timeout_hook),
            EscapedHelperExit::WaitError => request.wait = Some(&wait_error_hook),
        }
        if !with_cancel_pair {
            request.cancel_pair = Some(&failing_pair);
        }

        let started = std::time::Instant::now();
        let result = tracing::dispatcher::with_default(&dispatch, || spawn_with_timeout(request));
        let elapsed = started.elapsed();
        let events_at_return = events.load(Ordering::SeqCst);
        let case = format!("exit={exit:?} with_cancel_pair={with_cancel_pair}");

        assert!(
            process_is_live(fixture.helper_pid()),
            "{case}: the helper must escape the process-group kill"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "{case}: a busy escaped writer must not extend finalization; elapsed={elapsed:?}"
        );
        assert_eq!(
            live_readers.load(Ordering::SeqCst),
            0,
            "{case}: no output reader may outlive the supervisor"
        );
        assert!(
            events_at_return > 0,
            "{case}: the busy writer's output must reach the reader"
        );
        match exit {
            EscapedHelperExit::Normal => {
                let (stdout, _, exit_code, _, timed_out) = result.expect("normal exit");
                assert_eq!(exit_code, Some(0), "{case}");
                assert!(!timed_out, "{case}");
                assert!(stdout.observed_bytes() > 0, "{case}");
            }
            EscapedHelperExit::Timeout => {
                let (stdout, _, exit_code, _, timed_out) = result.expect("timeout");
                assert_eq!(exit_code, None, "{case}");
                assert!(timed_out, "{case}");
                assert!(stdout.observed_bytes() > 0, "{case}");
            }
            EscapedHelperExit::WaitError => {
                let error = result.expect_err("injected wait failure");
                assert!(error.message.contains("injected wait failure"), "{case}");
            }
        }

        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            events.load(Ordering::SeqCst),
            events_at_return,
            "{case}: no output line may be emitted after the supervisor returns"
        );
        drop(fixture);
    }
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
        // A helper that is still starting (for example when an assertion
        // failed early) records its PID shortly; wait for it rather than leak
        // the process.
        let mut pid = None;
        let _ = wait_until(Duration::from_secs(2), || {
            pid = read_pid_file(&self.pid_file);
            pid.is_some()
        });
        let Some(pid) = pid else {
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
fn read_pid_file(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
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
