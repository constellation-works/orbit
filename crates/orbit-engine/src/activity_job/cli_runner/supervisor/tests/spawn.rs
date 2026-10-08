use std::path::Path;
use std::time::Duration;

use super::super::super::tests::test_support::sh_args;
use super::super::{SpawnTraceContext, SpawnWithTimeoutRequest, spawn_with_timeout};
use super::test_support::{process_is_live, read_pid, spawn_test_request, stdin_trace, wait_until};

const STDIN_CHILD: &str =
    "activity_job::cli_runner::supervisor::tests::spawn::stdin_lifecycle_child";
const STDIN_DIR_ENV: &str = "ORBIT_TEST_STDIN_LIFECYCLE_DIR";
#[cfg(target_os = "linux")]
const STDIN_HOLDER: &str =
    "activity_job::cli_runner::supervisor::tests::spawn::escaped_stdin_holder";
#[cfg(target_os = "linux")]
const HOLDER_DIR_ENV: &str = "ORBIT_TEST_STDIN_HOLDER_DIR";
#[cfg(target_os = "linux")]
const STDIN_PROVIDER: &str = "activity_job::cli_runner::supervisor::tests::spawn::stdin_provider";
#[cfg(target_os = "linux")]
const PROVIDER_MODE_ENV: &str = "ORBIT_TEST_STDIN_PROVIDER_MODE";

/// Kernel pipe ownership and injected wait failures require the private
/// supervisor seam. Re-exec keeps process-wide thread/descriptor observations
/// and the Linux subreaper out of the parallel test runner.
#[test]
fn stdin_delivery_and_finalization_run_in_an_isolated_child() {
    let scratch = std::env::var_os("ORBIT_SCRATCH_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.orbit/tmp"));
    std::fs::create_dir_all(&scratch).expect("create fixture scratch");
    let dir = tempfile::Builder::new()
        .prefix("supervisor-stdin-")
        .tempdir_in(scratch)
        .expect("isolated stdin fixture directory");
    // The outer guard also cleans escaped helpers if the isolated test panics
    // or its bounded runner must kill it before Rust can run inner guards.
    #[cfg(target_os = "linux")]
    let _holders = StdinHolderGuard(dir.path().to_path_buf());
    let mut command = std::process::Command::new(std::env::current_exe().expect("test executable"));
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", STDIN_CHILD, "--ignored", "--nocapture"])
        .env(STDIN_DIR_ENV, dir.path());
    let output =
        orbit_common::process::run_bounded_capped(&mut command, Duration::from_secs(45), 64 * 1024)
            .expect("bounded isolated stdin fixture");
    orbit_common::test_env::assert_child_test_passed(
        STDIN_CHILD,
        output.status,
        output.stdout,
        output.stderr,
    );
}

#[test]
#[ignore = "entry point for the isolated stdin lifecycle fixture"]
fn stdin_lifecycle_child() {
    let root = std::env::var_os(STDIN_DIR_ENV).expect("isolated fixture directory");
    #[cfg(target_os = "linux")]
    {
        // SAFETY: only this isolated test process becomes a subreaper, so the
        // escaped descendants are ours to reap after their provider exits.
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
        orbit_common::test_env::assert_child_test_exists(STDIN_HOLDER);
        orbit_common::test_env::assert_child_test_exists(STDIN_PROVIDER);
    }
    let no_pair = || Err(std::io::Error::from_raw_os_error(libc::EMFILE));
    for without_wakeup in [false, true] {
        let cancel_pair = without_wakeup.then_some(&no_pair as _);
        // Exercise partial writes and EOF with an arbitrary oversized binary
        // payload; cat cannot exit successfully until it receives all bytes.
        for payload in [Vec::new(), (0..4 * 1024 * 1024).map(|n| n as u8).collect()] {
            let args = Vec::new();
            let (stdout, stderr, exit_code, _, timed_out) =
                spawn_with_timeout(SpawnWithTimeoutRequest {
                    stdin_bytes: &payload,
                    output_capture_limit: Some(payload.len() + 1),
                    cancel_pair,
                    ..spawn_test_request(
                        "/bin/cat",
                        &args,
                        None,
                        Duration::from_secs(10),
                        stdin_trace(),
                    )
                })
                .expect("normal stdin delivery");
            assert_eq!(
                stdout.bytes(),
                payload,
                "deliver the complete stdin payload"
            );
            assert!(stderr.bytes().is_empty());
            assert_eq!(exit_code, Some(0));
            assert!(!timed_out);
        }
        // An early stdin close still reports the provider's status/stderr.
        let args = sh_args("printf 'early exit\\n' >&2; exit 7");
        let payload = vec![b'x'; 4 * 1024 * 1024];
        let (_, stderr, code, _, timed_out) = spawn_with_timeout(SpawnWithTimeoutRequest {
            stdin_bytes: &payload,
            cancel_pair,
            ..spawn_test_request(
                "/bin/sh",
                &args,
                None,
                Duration::from_secs(10),
                stdin_trace(),
            )
        })
        .expect("broken stdin pipe preserves provider outcome");
        assert_eq!(code, Some(7));
        assert_eq!(stderr.bytes(), b"early exit\n");
        assert!(!timed_out);

        #[cfg(target_os = "linux")]
        exercise_escaped_stdin(Path::new(&root), without_wakeup);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = root;
}

#[cfg(target_os = "linux")]
fn exercise_escaped_stdin(root: &Path, without_wakeup: bool) {
    use std::time::Instant;

    assert!(
        wait_until(Duration::from_secs(1), || proc_entry_count(
            "/proc/self/task"
        ) == 2),
        "isolated libtest must have only its main and test threads before the leak checks"
    );
    let baseline_threads = proc_entry_count("/proc/self/task");
    let baseline_fds = proc_entry_count("/proc/self/fd");
    let no_pair = || Err(std::io::Error::from_raw_os_error(libc::EMFILE));
    for mode in ["exit", "timeout", "wait_error"] {
        for _ in 0..3 {
            let dir = tempfile::Builder::new()
                .prefix("holder-")
                .tempdir_in(root)
                .expect("holder directory");
            let _holder = StdinHolderGuard(dir.path().to_path_buf());
            let exe = std::env::current_exe().expect("test executable");
            let program = exe.to_str().expect("test executable path");
            let args = vec![
                "--exact".to_string(),
                STDIN_PROVIDER.to_string(),
                "--ignored".to_string(),
                "--nocapture".to_string(),
            ];
            let env = vec![
                (HOLDER_DIR_ENV.to_string(), dir.path().display().to_string()),
                (PROVIDER_MODE_ENV.to_string(), mode.to_string()),
            ];
            let wait_failure = |child: &mut std::process::Child| {
                if dir.path().join("filled").exists() {
                    Err(std::io::Error::other(
                        "injected wait failure after stdin filled",
                    ))
                } else {
                    child.try_wait()
                }
            };
            let payload = vec![b'x'; 4 * 1024 * 1024];
            let started = Instant::now();
            let result = spawn_with_timeout(SpawnWithTimeoutRequest {
                stdin_bytes: &payload,
                env: &env,
                wait: (mode == "wait_error").then_some(&wait_failure as _),
                cancel_pair: without_wakeup.then_some(&no_pair as _),
                ..spawn_test_request(
                    program,
                    &args,
                    None,
                    if mode == "timeout" {
                        Duration::from_secs(2)
                    } else {
                        Duration::from_secs(10)
                    },
                    stdin_trace(),
                )
            });
            assert!(
                dir.path().join("filled").exists(),
                "fixture must fill oversized stdin before {mode}: {result:?}"
            );
            match (mode, result) {
                ("wait_error", Err(error)) => {
                    assert!(!error.permanent);
                    assert!(error.message.contains("injected wait failure"));
                }
                ("exit", Ok((_, _, code, _, timed_out))) => {
                    assert_eq!(code, Some(0));
                    assert!(!timed_out);
                }
                ("timeout", Ok((_, _, code, _, timed_out))) => {
                    assert_eq!(code, None);
                    assert!(timed_out);
                }
                (_, other) => panic!("unexpected {mode} outcome: {other:?}"),
            }
            assert!(
                started.elapsed() < Duration::from_secs(if mode == "timeout" { 4 } else { 2 }),
                "stdin finalization must preserve the wall-clock and cleanup bounds"
            );
            assert_eq!(
                proc_entry_count("/proc/self/fd"),
                baseline_fds,
                "no stdin descriptor or cancel socket may survive {mode}"
            );
            assert!(
                wait_until(Duration::from_secs(1), || proc_entry_count(
                    "/proc/self/task"
                ) == baseline_threads),
                "no stdin thread or its owned prompt buffer may survive {mode}"
            );
            assert!(
                wait_until(Duration::from_secs(1), || dir
                    .path()
                    .join("released")
                    .exists()),
                "the escaped holder must observe stdin closure before fixture cleanup"
            );
            assert!(
                process_is_live(read_pid(&dir.path().join("holder.pid"))),
                "the holder must still be alive when finalization is verified"
            );
        }
    }
}

/// The provider synchronizes with its escaped helper before exiting. Rust
/// sleeps avoid shell polling children that would themselves need reaping.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "provider entry point for the isolated stdin fixture"]
#[allow(
    clippy::zombie_processes,
    reason = "the isolated fixture subreaper owns and reaps the escaped helper"
)]
fn stdin_provider() {
    use std::process::{Command, Stdio};

    let dir = std::env::var_os(HOLDER_DIR_ENV).expect("holder directory");
    let mode = std::env::var(PROVIDER_MODE_ENV).expect("provider mode");
    let _holder = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", STDIN_HOLDER, "--ignored", "--nocapture"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn escaped stdin holder");
    assert!(
        wait_until(Duration::from_secs(5), || Path::new(&dir)
            .join("filled")
            .exists()),
        "provider must wait for escaped holder to observe a full stdin pipe"
    );
    if mode != "exit" {
        std::thread::sleep(Duration::from_secs(90));
    }
}

#[cfg(target_os = "linux")]
fn proc_entry_count(path: &str) -> usize {
    std::fs::read_dir(path).expect("process entries").count()
}

/// Never consumes stdin. A full-pipe handshake removes scheduling races;
/// POLLHUP observes writer closure even while unread bytes remain buffered.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "escaped descendant entry point for the isolated stdin fixture"]
fn escaped_stdin_holder() {
    use std::time::Instant;

    let dir = std::env::var_os(HOLDER_DIR_ENV).expect("holder directory");
    let dir = Path::new(&dir);
    // SAFETY: this fixture process leaves its provider's process group.
    assert_ne!(unsafe { libc::setsid() }, -1, "escape provider session");
    std::fs::write(dir.join("holder.pid.tmp"), std::process::id().to_string()).expect("write pid");
    std::fs::rename(dir.join("holder.pid.tmp"), dir.join("holder.pid")).expect("publish pid");
    // SAFETY: FD 0 is the inherited fixture stdin pipe. These operations only
    // inspect its capacity and queued bytes, without reading or closing it.
    let capacity = unsafe { libc::fcntl(0, libc::F_GETPIPE_SZ) };
    assert!(capacity > 0 && capacity < 4 * 1024 * 1024);
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut filled = false;
    while Instant::now() < deadline {
        let mut queued: libc::c_int = 0;
        // SAFETY: FIONREAD writes one c_int through a valid pointer.
        assert_eq!(unsafe { libc::ioctl(0, libc::FIONREAD, &mut queued) }, 0);
        if !filled && queued == capacity {
            std::fs::write(dir.join("filled"), b"").expect("publish full pipe");
            filled = true;
        }
        let mut pipe = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll inspects the owned stdin without consuming it.
        assert!(unsafe { libc::poll(&mut pipe, 1, 0) } >= 0);
        if pipe.revents & libc::POLLHUP != 0 {
            assert!(filled, "stdin must fill before its writer is cancelled");
            std::fs::write(dir.join("released"), b"").expect("publish stdin closure");
            // Remain alive until the guard kills us, so finalization cannot
            // pass by waiting for this fixture to close its own read end.
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
struct StdinHolderGuard(std::path::PathBuf);

#[cfg(target_os = "linux")]
impl Drop for StdinHolderGuard {
    fn drop(&mut self) {
        // The outer guard walks the isolated fixture directories as a fallback
        // when the child process cannot run its per-holder guard.
        if let Ok(entries) = std::fs::read_dir(&self.0) {
            for entry in entries.flatten().filter(|entry| entry.path().is_dir()) {
                let _guard = Self(entry.path());
            }
        }
        let pid = std::fs::read_to_string(self.0.join("holder.pid"))
            .ok()
            .and_then(|pid| pid.parse::<libc::pid_t>().ok());
        if let Some(pid) = pid.filter(|pid| *pid > 1) {
            // SAFETY: only the fixture-owned holder publishes this pid. The
            // isolated child is a subreaper and can also reap it; the outer
            // fallback may receive ECHILD, which needs no further action.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, std::ptr::null_mut(), 0);
            }
        }
    }
}

/// A provider whose shell tool stops itself: the supervisor ends the stopped
/// descendant once the threshold passes, reports it, and the provider's wait
/// returns so the invocation finishes long before its wall clock.
#[cfg(target_os = "linux")]
#[test]
fn a_provider_blocked_on_a_stopped_descendant_finishes_once_the_supervisor_ends_it() {
    use super::super::StoppedDescendantReporter;
    use orbit_common::process::stopped_descendants::StoppedDescendant;

    let script = "sh -c 'kill -STOP $$; echo resumed' & child=$!; echo \"child=$child\"; \
                  wait $child; echo \"wait=$?\"; exit 0";
    let run = |threshold: Duration, timeout: Duration| {
        let args = sh_args(script);
        let reports = std::cell::RefCell::new(Vec::<StoppedDescendant>::new());
        let report = |stopped: &StoppedDescendant| reports.borrow_mut().push(stopped.clone());
        let outcome = spawn_with_timeout(SpawnWithTimeoutRequest {
            stopped_descendants: Some(StoppedDescendantReporter {
                threshold,
                report: &report,
            }),
            ..spawn_test_request(
                "/bin/sh",
                &args,
                None,
                timeout,
                SpawnTraceContext {
                    provider: "fixture",
                    job_run_id: "job-stopped-descendant",
                    task_id: None,
                    cwd: None,
                },
            )
        })
        .expect("spawn succeeds");
        (outcome, reports.into_inner())
    };

    let ((stdout, _, exit_code, duration, timed_out), reports) =
        run(Duration::from_secs(1), Duration::from_secs(60));
    let stdout = String::from_utf8_lossy(stdout.bytes()).into_owned();
    let child_pid: u32 = stdout
        .lines()
        .find_map(|line| line.strip_prefix("child="))
        .and_then(|pid| pid.parse().ok())
        .expect("the provider names its stopped child");
    assert_eq!(exit_code, Some(0), "{stdout}");
    assert!(!timed_out);
    assert!(stdout.contains("wait=137"), "{stdout}");
    assert!(duration < Duration::from_secs(30), "{duration:?}");
    let [stopped] = reports.as_slice() else {
        panic!("exactly the stopped child is reported: {reports:?}");
    };
    assert_eq!(stopped.pid, child_pid);
    assert_eq!(stopped.command, "sh -c kill -STOP $$; echo resumed");
    assert!(stopped.ended() && stopped.stopped_for >= Duration::from_secs(1));
    assert!(stopped.pid_start_time.is_some());

    // A deadline that lands first is a timeout, exactly as without the watch.
    let ((_, _, exit_code, _, timed_out), reports) =
        run(Duration::from_secs(30), Duration::from_millis(500));
    assert!(timed_out && exit_code.is_none());
    assert!(reports.is_empty(), "{reports:?}");
}
