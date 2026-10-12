use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use super::super::{CaptureSink, Outcome, Stdin, SupervisedChild, Supervision, Termination};

const OUTPUT_LIMIT: usize = 1024 * 1024;
const GRACE: Duration = Duration::from_secs(5);
const DRAIN_BUDGET: Duration = Duration::from_secs(1);

/// A process-group leader with piped stdout and stderr, and a piped stdin
/// when `stdin_piped`; otherwise stdin is null.
fn command(program: &str, args: &[&str], stdin_piped: bool) -> Command {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(if stdin_piped {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command
}

fn shell(script: &str) -> Child {
    command("/bin/sh", &["-c", script], false)
        .spawn()
        .expect("spawn supervised shell")
}

fn supervision<'a>(
    timeout: Option<Duration>,
    stdin: Stdin,
    output_limit: usize,
    cancelled: Option<&'a AtomicBool>,
    stopped_descendants: Option<Duration>,
) -> Supervision<'a> {
    Supervision {
        stdin,
        stdout: Box::new(CaptureSink::new("stdout", output_limit)),
        stderr: Box::new(CaptureSink::new("stderr", output_limit)),
        timeout,
        termination: Termination::Grace(GRACE),
        cancelled,
        intercept_signals: true,
        drain_budget: DRAIN_BUDGET,
        stopped_descendants,
    }
}

fn supervise(
    child: Child,
    timeout: Option<Duration>,
    stdin: Stdin,
    output_limit: usize,
) -> Outcome {
    SupervisedChild::adopt(child)
        .supervise(supervision(timeout, stdin, output_limit, None, None))
        .expect("supervised wait")
}

/// Fault injection is admitted here because setup errors cannot be requested
/// through the public API without exhausting descriptors or poisoning global
/// signal state. A separately waitable peer proves cleanup kills the group,
/// while waitpid(ECHILD) proves the supervisor reaped its direct child.
#[cfg(unix)]
#[test]
fn supervision_errors_kill_the_process_group_and_reap_the_child() {
    use std::os::unix::process::{CommandExt, ExitStatusExt};

    use wait_timeout::ChildExt;

    use super::super::super::OwnedChild;
    use super::super::RelaySink;
    use super::super::wait::{SupervisionFailure, with_supervision_failure};

    struct ProcessGroup {
        pid: Option<u32>,
        child: Option<Child>,
        peer: Option<Child>,
    }

    impl Drop for ProcessGroup {
        fn drop(&mut self) {
            if let Some(pid) = self.pid {
                let _ = OwnedChild::signal_spawned_group(pid, libc::SIGKILL);
                if self.child.is_none() {
                    // SAFETY: this is the child the fixture handed to the
                    // supervisor. Reap it if failed supervision did not.
                    unsafe { libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), 0) };
                }
            }
            for child in self.child.iter_mut().chain(self.peer.iter_mut()) {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    let cases = [
        (None, false), // Payload requested without a stdin pipe.
        (Some(SupervisionFailure::DrainStop), false),
        (Some(SupervisionFailure::Watch(0)), false), // Stdin.
        (Some(SupervisionFailure::Watch(1)), false), // Captured stdout.
        (Some(SupervisionFailure::Watch(1)), true),  // Relayed stdout.
        (Some(SupervisionFailure::Watch(2)), false), // Stderr, after two workers.
        (Some(SupervisionFailure::SignalInstall), false), // Before pipe setup.
        (Some(SupervisionFailure::Wait), false),     // Handler already installed.
    ];
    for (failure, relay) in cases {
        let payload = vec![b'x'; 1024 * 1024];
        let sleeper = || command("/bin/sleep", &["60"], failure.is_some());
        let child = sleeper().spawn().expect("spawn group leader");
        let pid = child.id();
        let mut group = ProcessGroup {
            pid: Some(pid),
            child: Some(child),
            peer: None,
        };
        // Unlike a grandchild orphaned by SIGKILL, this peer is ours to reap,
        // so the test does not depend on the host init process reaping it.
        group.peer = Some(
            sleeper()
                .process_group(pid as i32)
                .spawn()
                .expect("spawn peer in supervised process group"),
        );
        let child = group.child.take().expect("group leader");
        let supervise = || {
            let mut run = supervision(
                Some(Duration::from_secs(5)),
                Stdin::Payload(payload),
                OUTPUT_LIMIT,
                None,
                None,
            );
            let _reader;
            if relay {
                let (reader, writer) = std::io::pipe().expect("relay pipe");
                _reader = reader;
                run.stdout = Box::new(RelaySink::new(writer));
            }
            SupervisedChild::adopt(child).supervise(run).map(|_| ())
        };
        let result = match failure {
            Some(failure) => with_supervision_failure(failure, supervise),
            None => supervise(),
        };

        // SAFETY: queries only the child pid the fixture spawned. WNOHANG
        // distinguishes an unreaped live child (0) or zombie (pid) from ECHILD.
        let waited =
            unsafe { libc::waitpid(pid as libc::pid_t, std::ptr::null_mut(), libc::WNOHANG) };
        let wait_error = std::io::Error::last_os_error().raw_os_error();
        let peer_status = group
            .peer
            .as_mut()
            .expect("peer")
            .wait_timeout(Duration::from_secs(3))
            .expect("wait for peer");
        // SAFETY: signal zero only probes the fixture's process group.
        let group_probe = unsafe { libc::killpg(pid as libc::pid_t, 0) };
        let group_error = std::io::Error::last_os_error().raw_os_error();
        if group_probe == -1 && group_error == Some(libc::ESRCH) {
            // The group is gone; avoid signaling its id again in test cleanup.
            group.pid = None;
        }

        assert!(
            result.is_err(),
            "fixture must return an error: {failure:?}, relay={relay}"
        );
        assert_eq!(
            (waited, wait_error),
            (-1, Some(libc::ECHILD)),
            "supervision must reap its child on error: {failure:?}, relay={relay}"
        );
        assert_eq!(
            peer_status.and_then(|status| status.signal()),
            Some(libc::SIGKILL),
            "supervision must kill the whole group on error: {failure:?}, relay={relay}"
        );
        assert_eq!(
            (group_probe, group_error),
            (-1, Some(libc::ESRCH)),
            "no live process group may survive a supervision error: {failure:?}, relay={relay}"
        );
    }
}

/// Use the per-call limit seam to exercise pipe closure and capture reporting
/// without changing the process-wide capture-limit environment. Finite children
/// exit before supervision starts, leaving both pipes buffered for the workers.
#[cfg(unix)]
#[test]
fn capture_limits_are_reported_after_child_exit() {
    use super::super::ProcessStopCause;

    let cases: [(&str, bool, &[&str]); 5] = [
        ("exec yes", false, &["stdout"]),
        ("printf '%065d' 0", true, &["stdout"]),
        ("printf '%065d' 0 >&2", true, &["stderr"]),
        (
            "printf '%065d' 0; printf '%065d' 0 >&2",
            true,
            &["stderr", "stdout"],
        ),
        ("printf '%064d' 0; printf '%064d' 0 >&2", true, &[]),
    ];
    for (script, exited, limited_streams) in cases {
        let mut child = shell(script);
        if exited {
            assert!(child.wait().expect("finite child exit").success());
        }

        let result = supervise(child, Some(Duration::from_secs(5)), Stdin::Null, 64);
        let mut reported: Vec<&str> = result
            .stop_causes
            .iter()
            .filter_map(|cause| match cause {
                ProcessStopCause::OutputCaptureLimit { stream } => Some(*stream),
                // `yes` can die of SIGPIPE once its stream stops being read.
                _ => None,
            })
            .collect();
        reported.sort_unstable(); // Expected streams are listed sorted.
        assert_eq!(
            reported, limited_streams,
            "capture-limit reporting for {script:?}"
        );
        assert_eq!(
            result.success,
            limited_streams.is_empty(),
            "truncated output must fail even when the child exited successfully: {script:?}"
        );
        assert!(!result.timed_out, "capture must finish before the deadline");
    }
}

/// A backend that exits before reading its stdin (missing interpreter, empty
/// shim, launcher error) closes the pipe out from under the writer thread,
/// which observes EPIPE. That must be reported like any other non-zero exit
/// — exit status plus stderr tail — not surfaced as a bare "Broken pipe" I/O
/// error (ORB-13029: hit twice in the pulsar packaging spike).
#[cfg(unix)]
#[test]
fn stdin_broken_pipe_reports_exit_status_and_stderr() {
    // Larger than a pipe's kernel buffer (typically 64 KiB on Linux) so
    // `write_all` is still blocked on buffer space when the child exits,
    // reliably observing EPIPE instead of racing a write that already fully
    // buffered before the child closed its end.
    let payload = vec![b'x'; 4 * 1024 * 1024];
    let child = command(
        "/bin/sh",
        &["-c", "echo boom-stderr-tail >&2; exit 3"],
        true,
    )
    .spawn()
    .expect("spawn child");

    let result = supervise(
        child,
        Some(Duration::from_secs(5)),
        Stdin::Payload(payload),
        64 * 1024,
    );

    assert!(!result.success);
    assert_eq!(result.exit_code, Some(3));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("boom-stderr-tail"), "stderr was {stderr:?}");
}

/// Escaped-descendant fixtures. The supervised `/bin/sh` starts this test
/// binary as a background helper ([`escaped_pipe_holder`]), which calls
/// `setsid` — leaving the child's process group — and keeps the child's
/// stdout/stderr open. The shell waits for the helper's pid file
/// before its final step, so the helper has always escaped by then; the
/// [`HolderGuard`] kills it whatever the test outcome, and the helper also
/// exits on its own after [`HOLDER_LIFETIME`].
#[cfg(unix)]
mod escaped {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use std::process::Child;

    use super::super::super::Stdin;
    use super::{DRAIN_BUDGET, OUTPUT_LIMIT, command};

    const HOLDER_DIR_ENV: &str = "ORBIT_TEST_ESCAPED_HOLDER_DIR";
    const HOLDER_TEST: &str = "process::supervisor::tests::wait::escaped::escaped_pipe_holder";
    /// Backstop only: well past any bounded return, so a supervisor that
    /// waits for the holder instead of the budget fails the timing assertion.
    const HOLDER_LIFETIME: Duration = Duration::from_secs(60);
    /// Scheduling slack on top of the documented bound, for loaded CI hosts.
    const SLACK: Duration = Duration::from_secs(3);
    const TIMEOUT: Duration = Duration::from_secs(3);

    /// Holder side; a no-op unless launched by a fixture below.
    #[test]
    fn escaped_pipe_holder() {
        let Some(dir) = std::env::var_os(HOLDER_DIR_ENV).map(PathBuf::from) else {
            return;
        };
        // SAFETY: `setsid` only moves this process into a new session and
        // process group — out of the supervised child's group.
        assert_ne!(unsafe { libc::setsid() }, -1, "setsid");
        let pid_tmp = dir.join("holder.pid.tmp");
        std::fs::write(&pid_tmp, std::process::id().to_string()).expect("write pid");
        std::fs::rename(&pid_tmp, dir.join("holder.pid")).expect("publish pid");

        // Report once the supervisor's ends of the held pipes are closed.
        let deadline = Instant::now() + HOLDER_LIFETIME;
        let (mut stdout_open, mut stderr_open) = (true, true);
        while Instant::now() < deadline {
            stdout_open = stdout_open && write_probe(libc::STDOUT_FILENO);
            stderr_open = stderr_open && write_probe(libc::STDERR_FILENO);
            let released = !stdout_open && !stderr_open;
            if released {
                let _ = std::fs::write(dir.join("released"), b"");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn write_probe(fd: libc::c_int) -> bool {
        // SAFETY: writes one byte from a live buffer to a standard stream.
        let rc = unsafe { libc::write(fd, b".".as_ptr().cast(), 1) };
        rc == 1 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EPIPE)
    }

    /// Kills the escaped holder on every exit path, including panics.
    struct HolderGuard {
        dir: PathBuf,
    }

    impl Drop for HolderGuard {
        fn drop(&mut self) {
            let pid = std::fs::read_to_string(self.dir.join("holder.pid"))
                .ok()
                .and_then(|pid| pid.trim().parse::<libc::pid_t>().ok());
            if let Some(pid) = pid.filter(|pid| *pid > 1) {
                // SAFETY: signals the holder this fixture started; the pid
                // file is written only by that process.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }

    struct Fixture {
        // Field order is drop order: kill the holder before removing its dir.
        guard: HolderGuard,
        _dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().expect("tempdir");
            Self {
                guard: HolderGuard {
                    dir: dir.path().to_path_buf(),
                },
                _dir: dir,
            }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.guard.dir.join(name)
        }

        /// Start an escaped holder, mark both streams, then await timeout.
        fn spawn(&self) -> Child {
            crate::test_env::assert_child_test_exists(HOLDER_TEST);
            let script = format!(
                r#"{HOLDER_DIR_ENV}="$2" "$1" --exact {HOLDER_TEST} --nocapture &
i=0
while [ ! -s "$2/holder.pid" ]; do
  i=$((i + 1)); [ "$i" -gt 400 ] && exit 97
  sleep 0.05
done
printf 'pre-exit-stdout\n'
printf 'pre-exit-stderr\n' >&2
: > "$2/armed"
sleep 60"#
            );
            let exe = std::env::current_exe().expect("test executable");
            let exe = exe.display().to_string();
            let dir = self.guard.dir.display().to_string();
            command("/bin/sh", &["-c", &script, "sh", &exe, &dir], false)
                .spawn()
                .expect("spawn child")
        }

        fn assert_released(&self) {
            let limit = Instant::now() + Duration::from_secs(3);
            while !self.path("released").exists() {
                assert!(
                    Instant::now() < limit,
                    "the held pipe must be closed on the supervisor side by the time it returns"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn supervise() {
        let fixture = Fixture::new();
        let child = fixture.spawn();
        let started = Instant::now();
        let result = super::supervise(child, Some(TIMEOUT), Stdin::Null, OUTPUT_LIMIT);
        let elapsed = started.elapsed();
        let bound = TIMEOUT + DRAIN_BUDGET + SLACK;

        assert!(
            fixture.path("armed").exists(),
            "fixture never armed: exit {:?}, stderr {:?}",
            result.exit_code,
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            elapsed < bound,
            "supervision must return within the drain budget while an escaped \
             helper holds a pipe: took {elapsed:?}, bound {bound:?}"
        );
        assert!(result.drain_stopped, "the escaped helper held a pipe open");
        assert!(result.timed_out);
        let stdout = String::from_utf8_lossy(&result.stdout);
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(stdout.contains("pre-exit-stdout"), "stdout was {stdout:?}");
        assert!(stderr.contains("pre-exit-stderr"), "stderr was {stderr:?}");
        fixture.assert_released();
    }

    #[test]
    fn timeout_returns_within_drain_budget_while_escaped_helper_holds_output() {
        supervise();
    }
}

/// A descendant that stops itself (state `T`) is ended once it has stayed
/// stopped across the threshold, so the child's wait on it returns; nothing
/// else is ever signalled. A short threshold keeps each case to seconds.
/// Kernel process states are only observable on a real process tree.
#[cfg(target_os = "linux")]
mod stopped_descendants {
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    use super::super::super::super::identity::linux_process_stat;
    use super::super::super::{Outcome, Stdin, SupervisedChild};
    use super::{OUTPUT_LIMIT, shell, supervision};

    /// Writes the stopped shell's pid, then reports how `wait` on it ended.
    const SELF_STOPPING_CHILD: &str = "sh -c 'kill -STOP $$; echo resumed' & child=$!; echo \"child=$child\"; \
         wait $child; echo \"wait=$?\"; exit 0";

    fn supervise_with(
        child: Child,
        threshold: Duration,
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
    ) -> Outcome {
        SupervisedChild::adopt(child)
            .supervise(supervision(
                Some(timeout),
                Stdin::Null,
                OUTPUT_LIMIT,
                cancelled,
                Some(threshold),
            ))
            .expect("supervised wait")
    }

    fn supervise(script: &str, threshold: Duration, timeout_ms: u64) -> (Outcome, Duration) {
        let child = shell(script);
        let started = Instant::now();
        let result = supervise_with(child, threshold, Duration::from_millis(timeout_ms), None);
        (result, started.elapsed())
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    fn assert_nothing_ended(result: &Outcome, case: &str) {
        assert!(
            result.stopped_descendants.is_empty(),
            "{case}: the supervisor must not end anything: {:?}",
            result.stopped_descendants
        );
    }

    /// A process outside the supervised tree, in its own group, reaped on drop.
    struct Bystander(Child);

    impl Drop for Bystander {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn wait_for_state(pid: u32, state: char) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if linux_process_stat(pid).is_some_and(|stat| stat.state == state) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_descendant_stopped_past_the_threshold_is_ended_and_the_child_finishes() {
        let (result, elapsed) = supervise(SELF_STOPPING_CHILD, Duration::from_secs(1), 60_000);

        let stdout = text(&result.stdout);
        let stderr = text(&result.stderr);
        let child_pid = stdout
            .lines()
            .find_map(|line| line.strip_prefix("child="))
            .expect("the shell names its stopped child");
        assert!(
            result.success && result.exit_code == Some(0),
            "the supervised shell exits 0 once its wait returns: {result:?}",
            result = (result.exit_code, &stdout, &stderr)
        );
        assert!(
            stdout.contains("wait=137") && !stdout.contains("resumed"),
            "the stopped child is killed, never resumed: {stdout:?}"
        );
        assert!(!result.timed_out);
        assert!(
            elapsed < Duration::from_secs(30),
            "the stop must not hold the child until its deadline: {elapsed:?}"
        );
        let reports: Vec<String> = result
            .stopped_descendants
            .iter()
            .map(|stopped| stopped.describe())
            .collect();
        assert_eq!(
            reports,
            [format!(
                "stopped descendant pid={child_pid} command=`sh -c kill -STOP $$; echo resumed` stopped for at least 1s: ended"
            )],
            "the report names the pid, command and stop time"
        );
    }

    #[test]
    fn a_stopped_supervised_child_leaves_its_stopped_descendants_alone() {
        let child = shell("sleep 1 & wait $!; echo \"wait=$?\"; exit 0");
        let group = child.id() as libc::pid_t;
        let operator = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            // SAFETY: signals only the fixture's own unreaped process group.
            unsafe { libc::killpg(group, libc::SIGSTOP) };
            std::thread::sleep(Duration::from_millis(1_500));
            // SAFETY: as above.
            unsafe { libc::killpg(group, libc::SIGCONT) };
        });
        let result = supervise_with(
            child,
            Duration::from_millis(400),
            Duration::from_secs(30),
            None,
        );
        operator.join().expect("operator thread");

        assert_nothing_ended(&result, "whole group stopped");
        assert!(
            text(&result.stdout).contains("wait=0") && result.success,
            "the group resumes and finishes as if never watched: {:?}",
            text(&result.stdout)
        );
    }

    #[test]
    fn a_descendant_resumed_before_the_threshold_is_untouched() {
        let (result, _) = supervise(
            "sh -c 'kill -STOP $$; echo resumed' & child=$!; sleep 1; kill -CONT $child; \
             wait $child; echo \"wait=$?\"",
            Duration::from_secs(3),
            30_000,
        );

        assert_nothing_ended(&result, "short stop");
        let stdout = text(&result.stdout);
        assert!(
            stdout.contains("resumed") && stdout.contains("wait=0"),
            "the briefly stopped child resumes and exits normally: {stdout:?}"
        );
    }

    #[test]
    fn an_idle_running_descendant_is_untouched() {
        let (result, _) = supervise(
            "sleep 2 & wait $!; echo \"wait=$?\"",
            Duration::from_millis(400),
            30_000,
        );

        assert_nothing_ended(&result, "idle sleeper");
        assert!(text(&result.stdout).contains("wait=0"));
    }

    #[test]
    fn a_stopped_process_outside_the_supervised_tree_is_never_signalled() {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "kill -STOP $$"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let bystander = Bystander(command.spawn().expect("spawn bystander"));
        let pid = bystander.0.id();
        assert!(wait_for_state(pid, 'T'), "bystander must stop itself");

        let (result, _) = supervise("sleep 2", Duration::from_millis(400), 30_000);

        assert_nothing_ended(&result, "bystander outside the tree");
        assert!(
            linux_process_stat(pid).is_some_and(|stat| stat.state == 'T'),
            "a stopped process outside the tree stays stopped"
        );
    }

    #[test]
    fn deadline_and_cancellation_end_the_child_as_before() {
        // The deadline lands before the threshold: a timeout, not an ended
        // descendant.
        let (result, elapsed) = supervise(SELF_STOPPING_CHILD, Duration::from_secs(30), 500);
        assert!(result.timed_out && result.exit_code.is_none());
        assert!(result.stop_causes.is_empty(), "{:?}", result.stop_causes);
        assert_nothing_ended(&result, "deadline");
        assert!(elapsed < Duration::from_secs(10), "{elapsed:?}");

        let cancelled = AtomicBool::new(false);
        let child = shell(SELF_STOPPING_CHILD);
        let result = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(300));
                cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
            });
            supervise_with(
                child,
                Duration::from_secs(30),
                Duration::from_secs(30),
                Some(&cancelled),
            )
        });
        assert!(!result.success && result.exit_code.is_none() && !result.timed_out);
        assert_nothing_ended(&result, "cancellation");
    }
}
