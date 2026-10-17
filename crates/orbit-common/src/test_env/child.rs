use std::io::Write;

use super::{clear_inherited_authority, host_load, orphan_watchdog};

/// Require a successful re-exec to have run exactly one test, not merely exited
/// successfully with a missing or ignored exact filter (ORB-13911).
///
/// Pass captured libtest output, including its final summary. Checking the last
/// summary prevents a nested child's result from hiding an empty outer run.
/// A passing child's `DEFERRED:` and `SKIP:` notices are repeated on the parent's stderr,
/// so a test that deferred its sandbox-confined path inside an isolated child
/// still says so in the run's output [ORB-14334]. Completed `qa-scenario-pass:`
/// rows are forwarded too, so consolidated cases expose each behavioral row.
pub fn assert_child_test_passed(
    test_name: &str,
    status: std::process::ExitStatus,
    stdout: impl AsRef<[u8]>,
    stderr: impl AsRef<[u8]>,
) {
    let stdout = String::from_utf8_lossy(stdout.as_ref());
    let stderr = String::from_utf8_lossy(stderr.as_ref());
    assert!(
        status.success(),
        "child test `{test_name}` failed ({status}):\n{stdout}\n{stderr}"
    );
    let summary = stdout
        .lines()
        .rev()
        .find(|line| line.starts_with("test result: "));
    assert!(
        summary.is_some_and(|line| {
            line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;")
        }),
        "child test `{test_name}` did not run exactly once; missing or ignored entry point:\n{stdout}\n{stderr}"
    );
    let deferrals = stdout.lines().chain(stderr.lines()).filter(|line| {
        let line = line.trim_start();
        line.starts_with(orbit_types::workflow::HOST_TEST_DEFERRED_PREFIX)
            || line.starts_with("SKIP:")
            || line.starts_with("qa-scenario-pass: ")
    });
    for line in deferrals {
        // Bypass libtest capture, as the child's own notice did.
        let _ = writeln!(std::io::stderr(), "{}", line.trim());
    }
}

/// Retain a consolidated behavioral row's completion at the test-command boundary.
/// Call only after the row's assertions return; unavailable coverage must still
/// emit its skip or deferral notice, which invalidates QA execution credit.
pub fn record_scenario_pass(name: &str) {
    let _ = writeln!(std::io::stderr(), "qa-scenario-pass: {name}");
}

/// How long [`run_child_test`] lets a re-executed child test go without
/// printing anything before it kills the child and fails.
///
/// A hang guard, not a speed budget. A fixture slows by an order of magnitude
/// on a CPU-saturated host: a default-concurrency nextest run beside a busy
/// drain, or an instrumented `cargo llvm-cov` run. Fixed one- and two-minute
/// deadlines failed passing children there (ORB-14659). It is measured from
/// the child's latest output, and the window opens at the first byte, so
/// exec and libtest startup do not consume it (ORB-16411). A child that
/// reports progress (a [`FixtureProgress`] fixture) is not killed for being
/// slow while it still advances. Write-latency spikes on a loaded macOS host
/// stretched a progressing golden fixture past a deadline measured from its
/// start (ORB-15293). A child that has not printed yet is bounded only by
/// [`CHILD_TEST_CEILING`].
pub const CHILD_TEST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);

/// The longest [`run_child_test`] lets a child run, even one that keeps
/// printing. It stays below nextest's ten-minute termination in
/// `.config/nextest.toml`, so the parent still reports the child's output
/// instead of being killed silently.
pub const CHILD_TEST_CEILING: std::time::Duration = std::time::Duration::from_secs(540);

/// Run a re-executed child test with its output in files under `output_dir`.
///
/// Stdin is null and, on Unix, the child leads its own process group, which
/// is killed once the child has exited or overrun, or by a watchdog process
/// if this process dies first (nextest interrupting or timing out the test).
/// Pass the result to
/// [`assert_child_test_passed`]. Once a child has printed, a further silence
/// of [`CHILD_TEST_DEADLINE`] fails the caller. A child that has not printed
/// yet, or one that keeps printing, fails only at [`CHILD_TEST_CEILING`].
/// Either failure includes the host's load and everything the child printed
/// so far, so an overrun on a saturated host is told apart from a hang at
/// the point where it stopped.
pub fn run_child_test(
    command: &mut std::process::Command,
    test_name: &str,
    output_dir: &std::path::Path,
) -> crate::process::CapturedOutput {
    run_child_test_within(
        command,
        test_name,
        output_dir,
        CHILD_TEST_DEADLINE,
        CHILD_TEST_CEILING,
    )
}

/// [`run_child_test`] with an explicit `quiet` deadline and `ceiling`, so a
/// test can exercise both overrun paths by injecting short ones instead of
/// waiting out minutes. `quiet` opens at the child's first byte and then
/// measures the gap since the latest output. Before that byte only `ceiling`
/// applies.
pub fn run_child_test_within(
    command: &mut std::process::Command,
    test_name: &str,
    output_dir: &std::path::Path,
    quiet: std::time::Duration,
    ceiling: std::time::Duration,
) -> crate::process::CapturedOutput {
    let stdout_path = output_dir.join("child-test-stdout.log");
    let stderr_path = output_dir.join("child-test-stderr.log");
    let create = |path: &std::path::Path| {
        std::fs::File::create(path)
            .unwrap_or_else(|error| panic!("create {} for `{test_name}`: {error}", path.display()))
    };
    command
        .stdin(std::process::Stdio::null())
        .stdout(create(&stdout_path))
        .stderr(create(&stderr_path));
    crate::process::bounded::isolate_process_group(command);
    let mut child = crate::process::OwnedChild::from_spawned(
        command
            .spawn()
            .unwrap_or_else(|error| panic!("spawn child test `{test_name}`: {error}")),
    );
    let watchdog = orphan_watchdog::OrphanWatchdog::arm(child.pid());
    let started = std::time::Instant::now();
    let output_len = || {
        [&stdout_path, &stderr_path]
            .iter()
            .map(|path| std::fs::metadata(path).map_or(0, |metadata| metadata.len()))
            .sum::<u64>()
    };
    // Opens at the first byte. Exec and libtest startup precede that byte,
    // and a short injected deadline must not kill a child that has not yet
    // printed (ORB-16411). Silence after it, and the ceiling, still fail.
    let mut printed = 0u64;
    let mut last_output: Option<std::time::Instant> = None;
    let status = loop {
        match child.child_mut().try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(error) => panic!("wait for child test `{test_name}`: {error}"),
        }
        let now_printed = output_len();
        if now_printed != printed {
            printed = now_printed;
            last_output = Some(std::time::Instant::now());
        }
        if last_output.is_some_and(|at| at.elapsed() >= quiet) {
            break Err(format!("printed nothing for {quiet:?}"));
        }
        if started.elapsed() >= ceiling {
            let overrun = if last_output.is_none() {
                format!("printed nothing before its {ceiling:?} ceiling")
            } else {
                format!("still printing at its {ceiling:?} ceiling")
            };
            break Err(overrun);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    child.kill_and_reap();
    if let Some(watchdog) = watchdog {
        watchdog.disarm();
    }
    let read = |path: &std::path::Path| std::fs::read(path).unwrap_or_default();
    let (stdout, stderr) = (read(&stdout_path), read(&stderr_path));
    let status = match status {
        Ok(status) => status,
        Err(overrun) => panic!(
            "child test `{test_name}` was still running after {:?} and {overrun} ({}); \
             killed. Output so far:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            started.elapsed(),
            host_load(),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        ),
    };
    crate::process::CapturedOutput {
        status,
        stdout,
        stderr,
    }
}
/// How long a [`FixtureProgress`] fixture may go without advancing before it
/// fails naming the phase it was in.
///
/// Inside [`CHILD_TEST_DEADLINE`], so a fixture in an isolated child reports
/// its own phase before the parent kills it as silent.
pub const FIXTURE_STALL_DEADLINE: std::time::Duration =
    CHILD_TEST_DEADLINE.saturating_sub(std::time::Duration::from_secs(30));

/// The longest a [`FixtureProgress`] fixture may run, even one that keeps
/// advancing. Inside [`CHILD_TEST_CEILING`], so a fixture in an isolated child
/// reports its own phase before the parent kills it, and inside nextest's
/// ten-minute termination for a fixture that runs in process.
pub const FIXTURE_CEILING: std::time::Duration =
    CHILD_TEST_CEILING.saturating_sub(std::time::Duration::from_secs(30));

/// How long one fixture step, a readiness wait, a single CLI command or HTTP
/// request, may take before the fixture treats it as hung.
///
/// A hang guard, not a speed budget, for the reason [`CHILD_TEST_DEADLINE`]
/// gives: a handover, server start or `orbit workspace init` that takes
/// seconds on an idle host took over a minute on a saturated one, where fixed
/// 5-, 30- and 60-second step bounds failed passing fixtures (ORB-15238).
/// Inside [`FIXTURE_STALL_DEADLINE`], so a step in an isolated child fails
/// with its own message before the parent kills the child.
pub const FIXTURE_STEP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);
/// Fail a fixture whose command `what` was killed at its
/// [`FIXTURE_STEP_DEADLINE`] guard after running for `elapsed`.
///
/// Call it before asserting on the command's outcome: a killed command
/// reports an interrupted status and empty output, which would otherwise read
/// as a refusal or crash of the command under test.
pub fn assert_step_finished(
    what: &str,
    elapsed: std::time::Duration,
    status: &std::process::ExitStatus,
) {
    assert!(
        status.success() || elapsed < FIXTURE_STEP_DEADLINE,
        "`{what}` was still running at its {FIXTURE_STEP_DEADLINE:?} hang guard ({elapsed:.1?}, \
         {}); killed with {status}",
        host_load()
    );
}

/// Phase progress for a large fixture, such as one that seeds thousands of
/// tasks before it measures anything.
///
/// Without it, a stalled fixture prints only `running 1 test` before its
/// deadline, so a slow seed cannot be told from a slow measured section
/// (ORB-14818). Each phase is reported on stderr when it starts, at most every
/// few seconds while it advances, and when it ends. An [`advance`] that comes
/// later than the stall deadline after the previous one (or the phase start),
/// or past [`FIXTURE_CEILING`], panics with the phase, the items done, the
/// elapsed time and the host load. The deadline is measured from the last
/// progress so a fixture slowed by host load still passes while it advances
/// (ORB-15293).
///
/// [`advance`]: FixtureProgress::advance
pub struct FixtureProgress {
    fixture: String,
    deadline: std::time::Duration,
    started: std::time::Instant,
    phase: &'static str,
    phase_started: std::time::Instant,
    advanced: std::time::Instant,
    done: usize,
    total: usize,
    reported: std::time::Instant,
}

impl FixtureProgress {
    const REPORT_EVERY: std::time::Duration = std::time::Duration::from_secs(5);

    /// Track `fixture` under [`FIXTURE_STALL_DEADLINE`].
    pub fn start(fixture: &str) -> Self {
        Self::with_deadline(fixture, FIXTURE_STALL_DEADLINE)
    }

    /// Track `fixture`, failing an advance more than `deadline` after the
    /// previous progress.
    pub fn with_deadline(fixture: &str, deadline: std::time::Duration) -> Self {
        let now = std::time::Instant::now();
        Self {
            fixture: fixture.to_string(),
            deadline,
            started: now,
            phase: "setup",
            phase_started: now,
            advanced: now,
            done: 0,
            total: 0,
            reported: now,
        }
    }

    /// End the current phase and start `phase`, which will advance `total`
    /// times.
    pub fn phase(&mut self, phase: &'static str, total: usize) {
        self.report("done");
        let now = std::time::Instant::now();
        (self.phase, self.phase_started, self.done, self.total) = (phase, now, 0, total);
        self.advanced = now;
        self.report("started");
    }

    /// Count one item of the current phase; fail the fixture if the item
    /// took longer than its deadline or the fixture has run past
    /// [`FIXTURE_CEILING`].
    pub fn advance(&mut self) {
        self.done += 1;
        let elapsed = self.started.elapsed();
        let stalled = self.advanced.elapsed();
        self.advanced = std::time::Instant::now();
        assert!(
            stalled <= self.deadline && elapsed <= FIXTURE_CEILING,
            "fixture `{}` overran its {:?} stall deadline or {:?} ceiling in phase `{}`: \
             {}/{} done after {:.1?} in the phase, {:.1?} since the previous item, {:.1?} \
             in total ({})",
            self.fixture,
            self.deadline,
            FIXTURE_CEILING,
            self.phase,
            self.done,
            self.total,
            self.phase_started.elapsed(),
            stalled,
            elapsed,
            host_load()
        );
        if self.reported.elapsed() >= Self::REPORT_EVERY {
            self.report("running");
        }
    }

    /// End the last phase.
    pub fn finish(mut self) {
        self.report("done");
    }

    fn report(&mut self, state: &str) {
        self.reported = std::time::Instant::now();
        // Bypass libtest capture: an overrun's kill must not lose the line.
        let _ = writeln!(
            std::io::stderr(),
            "fixture `{}`: phase `{}` {state}, {}/{} after {:.1?} ({:.1?} total)",
            self.fixture,
            self.phase,
            self.done,
            self.total,
            self.phase_started.elapsed(),
            self.started.elapsed()
        );
    }
}

/// Verify the exact entry point before starting a child that will be killed or
/// stays alive until a readiness handshake, so it cannot emit a final summary.
///
/// The caller must still verify a sentinel or handshake written by the child.
/// This probe lists tests without executing fixture code, including ignored
/// entries; callers must select ignored children with `--ignored` themselves.
pub fn assert_child_test_exists(test_name: &str) {
    let exe = std::env::current_exe()
        .unwrap_or_else(|error| panic!("locate child test `{test_name}` executable: {error}"));
    let mut command = std::process::Command::new(exe);
    clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command.args(["--list", "--exact", test_name, "--include-ignored"]);
    let output = crate::process::run_bounded_capped(
        &mut command,
        std::time::Duration::from_secs(30),
        64 * 1024,
    )
    .unwrap_or_else(|error| panic!("list child test `{test_name}`: {error}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let entry = format!("{test_name}: test");
    assert!(
        output.status.success() && stdout.lines().any(|line| line == entry),
        "missing child test entry point `{test_name}`:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
