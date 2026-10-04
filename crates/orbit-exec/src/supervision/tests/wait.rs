use super::super::wait::wait_with_timeout_and_output_limit;
use crate::runner::{EnvironmentMode, ExecRequest, StdinMode};

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

    use super::super::super::tee::DRAIN_BUDGET;
    use super::super::super::wait::{WaitResult, wait_with_timeout_and_output_limit};
    use crate::runner::{EnvironmentMode, ExecRequest, StdinMode};

    const HOLDER_DIR_ENV: &str = "ORBIT_TEST_ESCAPED_HOLDER_DIR";
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
        fn request(&self) -> ExecRequest {
            orbit_common::test_env::assert_child_test_exists(HOLDER_TEST);
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
            ExecRequest {
                program: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    script,
                    "sh".to_string(),
                    exe.display().to_string(),
                    self.guard.dir.display().to_string(),
                ],
                current_dir: None,
                timeout_ms: Some(TIMEOUT.as_millis() as u64),
                stdin_mode: StdinMode::Null,
                environment_mode: EnvironmentMode::Inherit,
                debug: false,
            }
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
        let req = fixture.request();
        let child = crate::process::spawn(&req).expect("spawn child");
        let started = Instant::now();
        let result: WaitResult =
            wait_with_timeout_and_output_limit(child, req.timeout_ms, false, None, 1024 * 1024)
                .expect("supervised wait");
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
