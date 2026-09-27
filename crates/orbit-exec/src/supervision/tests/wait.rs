use std::time::{Duration, Instant};

use orbit_common::process::output_capture::OUTPUT_TRUNCATED_MARKER;

use super::super::wait::{WaitResult, wait_with_timeout_and_output_limit};
use crate::runner::{EnvironmentMode, ExecRequest, StdinMode};

#[cfg(unix)]
#[test]
fn wait_kills_process_when_stdout_capture_limit_is_exceeded() {
    let req = ExecRequest {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "yes orbit-cap".to_string()],
        current_dir: None,
        timeout_ms: Some(5_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::Inherit,
        debug: false,
    };
    let child = crate::process::spawn(&req).expect("spawn child");

    let started = Instant::now();
    let result =
        wait_with_timeout_and_output_limit(child, Some(5_000), false, None, 64).expect("wait");

    assert!(!result.exit_success);
    // Hitting the output cap terminates the process two valid ways, decided by
    // thread scheduling: the supervisor may observe the cap signal first and
    // SIGKILL the group (exit_code None), or — under load — be parked in
    // child.wait_timeout when /bin/sh self-exits 141 because `yes` took SIGPIPE
    // the instant the drain thread dropped the pipe read end (128 + 13 = 141).
    // Both promptly kill the process and cap output (asserted below); only the
    // reported exit_code differs by who delivered the kill.
    assert!(
        result.exit_code.is_none() || result.exit_code == Some(141),
        "expected None (SIGKILL) or Some(141) (SIGPIPE cascade), got {:?}",
        result.exit_code
    );
    // `yes` may die of SIGPIPE before the supervisor observes the cap; only
    // the supervisor-delivered kill carries the reason (asserted separately
    // below with a child that survives the pipe closing).
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.is_empty() || stderr.contains("process output capture limit exceeded on stdout"),
        "stderr was {stderr:?}"
    );
    assert!(result.stdout.ends_with(OUTPUT_TRUNCATED_MARKER));
    assert!(result.stdout.len() <= 64 + OUTPUT_TRUNCATED_MARKER.len());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "output cap should kill the subprocess promptly"
    );
}

/// A child that ignores SIGPIPE keeps running after the drain drops the pipe,
/// so the supervisor is the one that stops it — and it must say why, or the
/// caller sees a bare failure with an empty stderr for a log that was merely
/// long.
#[cfg(unix)]
#[test]
fn capture_limit_kill_names_its_reason_on_stderr() {
    let req = ExecRequest {
        program: "/bin/sh".to_string(),
        args: vec![
            "-c".to_string(),
            "trap '' PIPE; while :; do echo orbit-cap; done".to_string(),
        ],
        current_dir: None,
        timeout_ms: Some(5_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::Inherit,
        debug: false,
    };
    let child = crate::process::spawn(&req).expect("spawn child");

    let result =
        wait_with_timeout_and_output_limit(child, Some(5_000), false, None, 64).expect("wait");

    assert!(!result.exit_success);
    assert_eq!(result.exit_code, None, "supervisor kill, not a self-exit");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("process output capture limit exceeded on stdout"),
        "stderr was {stderr:?}"
    );
    assert!(result.stdout.ends_with(OUTPUT_TRUNCATED_MARKER));
}

/// Concurrent supervisors must overlap: the process-wide signal-handler
/// mutex used to be held for each child's entire lifetime, so two `sleep 1`
/// waits from two threads took ~2 s instead of ~1 s.
#[cfg(unix)]
#[test]
fn concurrent_supervised_sleeps_overlap() {
    let started = Instant::now();
    std::thread::scope(|scope| {
        let first = scope.spawn(|| supervise_sleep("1"));
        let second = scope.spawn(|| supervise_sleep("1"));
        let first = first
            .join()
            .expect("first supervisor thread")
            .expect("first wait");
        let second = second
            .join()
            .expect("second supervisor thread")
            .expect("second wait");
        assert!(first.exit_success, "first sleep should exit 0");
        assert!(second.exit_success, "second sleep should exit 0");
    });
    assert!(
        started.elapsed() < Duration::from_millis(1_500),
        "concurrent sleep 1 children serialized: {:?}",
        started.elapsed()
    );
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
    let req = ExecRequest {
        program: "/bin/sh".to_string(),
        args: vec![
            "-c".to_string(),
            "echo boom-stderr-tail >&2; exit 3".to_string(),
        ],
        current_dir: None,
        timeout_ms: Some(5_000),
        stdin_mode: StdinMode::Bytes(payload.clone()),
        environment_mode: EnvironmentMode::Inherit,
        debug: false,
    };
    let child = crate::process::spawn(&req).expect("spawn child");

    let result =
        wait_with_timeout_and_output_limit(child, Some(5_000), false, Some(payload), 64 * 1024)
            .expect("wait");

    assert!(!result.exit_success);
    assert_eq!(result.exit_code, Some(3));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("boom-stderr-tail"), "stderr was {stderr:?}");
}

#[cfg(unix)]
fn supervise_sleep(seconds: &str) -> Result<WaitResult, orbit_common::OrbitError> {
    let req = ExecRequest {
        program: "/bin/sleep".to_string(),
        args: vec![seconds.to_string()],
        current_dir: None,
        timeout_ms: Some(5_000),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::Inherit,
        debug: false,
    };
    let child = crate::process::spawn(&req).expect("spawn child");
    wait_with_timeout_and_output_limit(child, Some(5_000), false, None, 64)
}

/// Escaped-descendant fixtures. The supervised `/bin/sh` starts this test
/// binary as a background helper ([`escaped_pipe_holder`]), which calls
/// `setsid` — leaving the child's process group — and keeps the child's
/// stdout/stderr (or stdin) open. The shell waits for the helper's pid file
/// before its final step, so the helper has always escaped by then; the
/// [`HolderGuard`] kills it whatever the test outcome, and the helper also
/// exits on its own after [`HOLDER_LIFETIME`].
#[cfg(unix)]
mod escaped {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use super::super::super::tee::DRAIN_BUDGET;
    use super::super::super::wait::{
        WaitResult, wait_with_cancellation, wait_with_timeout_and_output_limit,
    };
    use crate::runner::{EnvironmentMode, ExecRequest, StdinMode, run_process_streaming_stdout};

    const HOLDER_DIR_ENV: &str = "ORBIT_TEST_ESCAPED_HOLDER_DIR";
    const HOLDER_MODE_ENV: &str = "ORBIT_TEST_ESCAPED_HOLDER_MODE";
    const HOLDER_TEST: &str = "supervision::tests::wait::escaped::escaped_pipe_holder";
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
        let holds_stdin = std::env::var(HOLDER_MODE_ENV).as_deref() == Ok("stdin");
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
            let released = if holds_stdin {
                stdin_hung_up()
            } else {
                stdout_open = stdout_open && write_probe(libc::STDOUT_FILENO);
                stderr_open = stderr_open && write_probe(libc::STDERR_FILENO);
                !stdout_open && !stderr_open
            };
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

    fn stdin_hung_up() -> bool {
        let mut fd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: polls one valid `pollfd` without blocking.
        let rc = unsafe { libc::poll(&mut fd, 1, 0) };
        rc > 0 && fd.revents & libc::POLLHUP != 0
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

    #[derive(Clone, Copy, PartialEq)]
    enum Held {
        Output,
        Stdin,
    }

    #[derive(Clone, Copy, PartialEq)]
    enum End {
        Exit,
        Timeout,
        Cancel,
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

        /// A shell that starts the holder, waits until it has escaped, marks
        /// pre-completion output on both streams, then exits or stays up.
        fn request(&self, held: Held, end: End) -> ExecRequest {
            let redirect = match held {
                Held::Output => "",
                // A background job's stdin is /dev/null unless redirected
                // explicitly, so hand it the original through fd 3.
                Held::Stdin => "<&3 >/dev/null 2>&1",
            };
            let finish = match end {
                End::Exit => "exit 0",
                End::Timeout | End::Cancel => "sleep 60",
            };
            let script = format!(
                r#"exec 3<&0
{HOLDER_DIR_ENV}="$2" {HOLDER_MODE_ENV}="$3" "$1" --exact {HOLDER_TEST} --nocapture {redirect} &
exec 3<&-
i=0
while [ ! -s "$2/holder.pid" ]; do
  i=$((i + 1)); [ "$i" -gt 400 ] && exit 97
  sleep 0.05
done
printf 'pre-exit-stdout\n'
printf 'pre-exit-stderr\n' >&2
: > "$2/armed"
{finish}"#
            );
            let exe = std::env::current_exe().expect("test executable");
            ExecRequest {
                program: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    script,
                    "sh".to_string(),
                    exe.display().to_string(),
                    self.guard.dir.display().to_string(),
                    match held {
                        Held::Output => "output",
                        Held::Stdin => "stdin",
                    }
                    .to_string(),
                ],
                current_dir: None,
                timeout_ms: Some(TIMEOUT.as_millis() as u64),
                stdin_mode: StdinMode::Null,
                environment_mode: EnvironmentMode::Inherit,
                debug: false,
            }
        }

        /// Poll until the shell reports the holder escaped and the
        /// pre-completion output written, or the supervised call returned.
        fn wait_armed(&self, finished: impl Fn() -> bool) -> Instant {
            let limit = Instant::now() + Duration::from_secs(30);
            while !self.path("armed").exists() && !finished() && Instant::now() < limit {
                std::thread::sleep(Duration::from_millis(10));
            }
            Instant::now()
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

    fn supervise(held: Held, end: End) {
        let fixture = Fixture::new();
        let mut req = fixture.request(held, end);
        // Larger than any pipe buffer, so the writer is still blocked on the
        // holder when the child ends.
        let payload = (held == Held::Stdin).then(|| vec![b'x'; 4 * 1024 * 1024]);
        if let Some(bytes) = &payload {
            req.stdin_mode = StdinMode::Bytes(bytes.clone());
        }
        let child = crate::process::spawn(&req).expect("spawn child");
        let cancelled = AtomicBool::new(false);
        let started = Instant::now();

        let (result, armed_at, returned_at) = std::thread::scope(|scope| {
            let supervisor = scope.spawn(|| -> WaitResult {
                let result = if end == End::Cancel {
                    wait_with_cancellation(child, req.timeout_ms, payload, Some(&cancelled))
                } else {
                    wait_with_timeout_and_output_limit(
                        child,
                        req.timeout_ms,
                        false,
                        payload,
                        1024 * 1024,
                    )
                };
                result.expect("supervised wait")
            });
            let armed_at = fixture.wait_armed(|| supervisor.is_finished());
            if end == End::Cancel {
                cancelled.store(true, Ordering::SeqCst);
            }
            let result = supervisor.join().expect("supervisor thread");
            (result, armed_at, Instant::now())
        });

        assert!(
            fixture.path("armed").exists(),
            "fixture never armed: exit {:?}, stderr {:?}",
            result.exit_code,
            String::from_utf8_lossy(&result.stderr)
        );
        // Measured from the event that ends the child: its own exit or the
        // cancel, both at arming; the deadline, counted from spawn.
        let (elapsed, bound) = match end {
            End::Exit | End::Cancel => (returned_at - armed_at, DRAIN_BUDGET + SLACK),
            End::Timeout => (returned_at - started, TIMEOUT + DRAIN_BUDGET + SLACK),
        };
        assert!(
            elapsed < bound,
            "supervision must return within the drain budget while an escaped \
             helper holds a pipe: took {elapsed:?}, bound {bound:?}"
        );
        assert!(result.drain_stopped, "the escaped helper held a pipe open");
        match end {
            End::Exit => assert_eq!(result.exit_code, Some(0)),
            End::Timeout => assert!(result.timed_out),
            End::Cancel => assert_eq!(result.exit_code, None),
        }
        let stdout = String::from_utf8_lossy(&result.stdout);
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(stdout.contains("pre-exit-stdout"), "stdout was {stdout:?}");
        assert!(stderr.contains("pre-exit-stderr"), "stderr was {stderr:?}");
        fixture.assert_released();
    }

    #[test]
    fn exit_returns_within_drain_budget_while_escaped_helper_holds_output() {
        supervise(Held::Output, End::Exit);
    }

    #[test]
    fn timeout_returns_within_drain_budget_while_escaped_helper_holds_output() {
        supervise(Held::Output, End::Timeout);
    }

    #[test]
    fn cancel_returns_within_drain_budget_while_escaped_helper_holds_output() {
        supervise(Held::Output, End::Cancel);
    }

    #[test]
    fn exit_returns_within_drain_budget_while_escaped_helper_holds_stdin() {
        supervise(Held::Stdin, End::Exit);
    }

    #[test]
    fn timeout_returns_within_drain_budget_while_escaped_helper_holds_stdin() {
        supervise(Held::Stdin, End::Timeout);
    }

    #[test]
    fn cancel_returns_within_drain_budget_while_escaped_helper_holds_stdin() {
        supervise(Held::Stdin, End::Cancel);
    }

    /// The streaming consumer reads a relay the supervisor closes within the
    /// same bound, so it sees EOF and keeps what was written before exit.
    #[test]
    fn streaming_stdout_ends_within_drain_budget_while_escaped_helper_holds_it() {
        let fixture = Fixture::new();
        let req = fixture.request(Held::Output, End::Exit);

        let (outcome, armed_at, returned_at) = std::thread::scope(|scope| {
            let supervisor = scope.spawn(|| {
                run_process_streaming_stdout(&req, &crate::NoSandbox, |mut stdout| {
                    use std::io::Read;
                    let mut seen = Vec::new();
                    stdout
                        .read_to_end(&mut seen)
                        .map_err(|err| orbit_common::OrbitError::Execution(err.to_string()))?;
                    Ok(seen)
                })
            });
            let armed_at = fixture.wait_armed(|| supervisor.is_finished());
            let outcome = supervisor.join().expect("supervisor thread");
            (outcome, armed_at, Instant::now())
        });

        let (result, seen) = outcome.expect("streaming run");
        assert!(fixture.path("armed").exists(), "fixture never armed");
        let elapsed = returned_at - armed_at;
        assert!(
            elapsed < DRAIN_BUDGET + SLACK,
            "streaming stdout must end within the drain budget: took {elapsed:?}"
        );
        assert_eq!(result.exit_code, Some(0));
        let seen = String::from_utf8_lossy(&seen);
        assert!(seen.contains("pre-exit-stdout"), "consumer saw {seen:?}");
        fixture.assert_released();
    }
}
