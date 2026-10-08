//! Ambient-process isolation and validation of re-executed child tests.
//!
//! Environment variables and umask need isolation because a test that depends
//! on ambient process state passes or fails according to *how the suite was
//! launched* rather than what the code does. See [`unset`] for inherited env
//! vars and [`harden_dir`] for umask-derived directory permissions.
//!
//! Several Orbit surfaces read agent identity and run context from the
//! process environment — notably runtime actor identity and `tool run`
//! audit-role resolution. A test that asserts the *absence* of that context
//! ("attributes to the human actor", "falls back to the agent role") is only
//! correct when those variables are genuinely unset.
//!
//! GitHub CI runs the suite from a bare shell, so the assertions hold there by
//! accident. An agent running the same suite from inside a managed Orbit run
//! inherits `ORBIT_RUN_ID`, `ORBIT_AGENT_MODEL`, `ORBIT_TASK_ID`, … and those
//! tests flip to red for a reason that has nothing to do with the code under
//! test (ORB-10350). [`unset`] makes the expectation explicit instead of
//! ambient.
//!
//! Integration tests that *spawn* the `orbit` binary need the same defense
//! one process out, where the stakes include durable routing rather than just
//! attribution. [`INHERITED_AUTHORITY_ENV`] is the canonical list for that
//! case and [`clear_inherited_authority`] applies it.
//!
//! Always available so integration tests and sibling crates share one
//! implementation without changing `orbit-common`'s feature set. Child-test
//! guards reject successful libtest exits that never executed the exact filter.

use std::io::Write;
use std::sync::{
    Mutex, MutexGuard, OnceLock,
    atomic::{AtomicUsize, Ordering},
};

static ACTIVE_SCOPED_ENVS: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn scoped_env_active() -> bool {
    ACTIVE_SCOPED_ENVS.load(Ordering::SeqCst) != 0
}

/// The system temporary directory with every symlink resolved.
///
/// macOS points `TMPDIR` under `/var`, a symlink to `/private/var`, so a
/// `tempfile::tempdir()` root is spelled through a link. Orbit resolves
/// symlinks in the roots it is given (discovery lists a canonical directory and
/// reports canonical paths), so a fixture that compares those paths with the
/// spelled root, or hands a path to code that must see one physical spelling,
/// passes on Linux and fails on macOS. Create such a fixture's root with
/// `tempfile::tempdir_in(canonical_temp_dir())` instead of overriding `TMPDIR`
/// for the whole suite: fixtures that do not care keep the ordinary root, so a
/// symlinked ancestor keeps being exercised where it should be.
pub fn canonical_temp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir();
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

/// The identity pair consulted when a command carries no explicit
/// `--agent`/`--model` and no input attribution.
pub const AGENT_IDENTITY_ENV: &[&str] = &["ORBIT_AGENT_NAME", "ORBIT_AGENT_MODEL", "ORBIT_ACTOR"];

/// The variables an `orbit-engine` managed run exports into every spawned
/// activity (see `orbit-engine/src/context/env.rs`): job/task/session
/// identity plus the "this is a managed run" marker. A test that asserts
/// unmanaged-run behavior (default human attribution, an unleased audit
/// context, …) is only correct when these are genuinely unset — a child of a
/// managed Orbit run inherits them all (ORB-10436).
pub const MANAGED_RUN_ENV: &[&str] = &[
    "ORBIT_RUN_ID",
    "ORBIT_TASK_ID",
    "ORBIT_ACTIVE_TASK_ID",
    "ORBIT_SESSION_ID",
    "ORBIT_MANAGED_RUN_CONTEXT",
    "ORBIT_WORKSPACE",
];

/// Every ambient variable a test-spawned `orbit` child process must not
/// inherit from the process that launched the suite.
///
/// [`unset`] and [`MANAGED_RUN_ENV`] defend *in-process* tests. A test that
/// spawns the `orbit` binary needs the same defense one level out, and the
/// stakes are higher: the child re-reads the environment from scratch, and
/// two of these variables are durable routing authority.
/// `ORBIT_REGISTRY_ROOT` selects the host-global registry regardless of
/// `HOME`, and `ORBIT_WORKSPACE` selects a registered workspace inside it
/// (see `orbit-core/src/runtime/resolve.rs` and
/// `orbit-cmd/src/registry_runtime.rs`). Both are honored only behind the
/// managed-run trust boundary — but an agent running the suite from inside a
/// managed Orbit run supplies exactly that boundary, so a fixture that resets
/// only `HOME`/`ORBIT_ROOT` still routes its writes into the *live* workspace.
/// That is not hypothetical: it created three real task records before it was
/// caught (ORB-11300).
///
/// Git's repository locators also outrank cwd. Scrub Git setup commands as
/// well as Orbit children: otherwise `git init` can initialize the parent's
/// repository, and a workflow worker can acquire the parent's fetch lock even
/// though its home and Orbit authority were isolated (ORB-13940).
///
/// Clearing the whole set — routing, managed-run trust, actor identity,
/// inherited sandbox grants, and the plugin broker socket — makes a fixture's
/// authority a property of the fixture rather than of how the suite was
/// launched. Apply it with [`clear_inherited_authority`] *before* any variable
/// a test sets on purpose, so the deliberate value wins.
pub const INHERITED_AUTHORITY_ENV: &[&str] = &[
    // Git repository and write destinations override a fixture's cwd.
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    // Durable routing: which registry, workspace, and data root the child
    // writes to. `ORBIT_REGISTRY_ROOT` and `ORBIT_WORKSPACE` outrank `HOME`.
    "ORBIT_ROOT",
    "ORBIT_REGISTRY_ROOT",
    "ORBIT_WORKSPACE",
    "ORBIT_WORKSPACE_CLAIM_TOKEN",
    "ORBIT_WORKTREE_ROOT",
    "ORBIT_JOB_DIR",
    "ORBIT_ACTIVITY_DIR",
    // Managed-run trust boundary and run identity. Clearing the marker alone
    // would be enough to disarm routing today; clearing the whole envelope
    // keeps the fixture correct if that coupling ever changes.
    "ORBIT_MANAGED_RUN_CONTEXT",
    "ORBIT_WORKER_CONTEXT_REQUIRED",
    "ORBIT_RUN_ID",
    "ORBIT_TASK_ID",
    "ORBIT_ACTIVE_TASK_ID",
    "ORBIT_SESSION_ID",
    "ORBIT_ACTIVITY_ID",
    "ORBIT_STEP_INDEX",
    // Actor identity and audit role attributed to the child's writes.
    "ORBIT_AGENT_NAME",
    "ORBIT_AGENT_MODEL",
    "ORBIT_ACTOR",
    "ORBIT_OPERATOR",
    "ORBIT_TASK_ACTOR_KIND",
    // Sandbox and tool grants leased to the host activity, not to a fixture.
    "ORBIT_ACTIVITY_TOOLS",
    "ORBIT_ACTIVITY_TOOL_POLICY",
    "ORBIT_ACTIVITY_TOOLS_DENY",
    "ORBIT_ACTIVITY_NAME",
    "ORBIT_ACTIVITY_FS_PROFILE",
    "ORBIT_ACTIVITY_DEADLINE_UNIX_MS",
    "ORBIT_PROC_ALLOWED_PROGRAMS",
    "ORBIT_PROC_PROGRAM_POLICY",
    "ORBIT_PROC_DISALLOWED_PROGRAMS",
    "ORBIT_BIN",
    // Routes plugin tool calls to the enclosing run's host broker instead of
    // the fixture's own install; a fixture that needs a broker sets its own.
    "ORBIT_PLUGIN_BROKER",
];

/// Clear every [`INHERITED_AUTHORITY_ENV`] variable from a child command.
///
/// `clear` is the command builder's `env_remove`. Passing it as a closure
/// keeps this crate free of a test-harness dependency while still giving every
/// fixture one shared list to drift against:
///
/// ```ignore
/// let mut command = cargo_bin_cmd!("orbit");
/// test_env::clear_inherited_authority(|name| {
///     command.env_remove(name);
/// });
/// command.current_dir(work).env("HOME", home);
/// ```
pub fn clear_inherited_authority(mut clear: impl FnMut(&str)) {
    for name in INHERITED_AUTHORITY_ENV {
        clear(name);
    }
}

/// Require a successful re-exec to have run exactly one test, not merely exited
/// successfully with a missing or ignored exact filter (ORB-13911).
///
/// Pass captured libtest output, including its final summary. Checking the last
/// summary prevents a nested child's result from hiding an empty outer run.
/// A passing child's `DEFERRED:` notices are repeated on the parent's stderr,
/// so a test that deferred its sandbox-confined path inside an isolated child
/// still says so in the run's output [ORB-14334].
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
        line.trim_start()
            .starts_with(orbit_types::workflow::HOST_TEST_DEFERRED_PREFIX)
    });
    for line in deferrals {
        // Bypass libtest capture, as the child's own notice did.
        let _ = writeln!(std::io::stderr(), "{}", line.trim());
    }
}

/// How long [`run_child_test`] lets a re-executed child test run before it
/// kills the child and fails.
///
/// A hang guard, not a speed budget. A fixture slows by an order of magnitude
/// on a CPU-saturated host: a default-concurrency nextest run beside a busy
/// drain, or an instrumented `cargo llvm-cov` run. Fixed one- and two-minute
/// deadlines failed passing children there (ORB-14659). The guard stays below
/// nextest's ten-minute termination in `.config/nextest.toml`, so the parent
/// still reports the child's output instead of being killed silently.
pub const CHILD_TEST_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);

/// Run a re-executed child test with its output in files under `output_dir`.
///
/// Stdin is null and, on Unix, the child leads its own process group, which
/// is killed once the child has exited or overrun. Pass the result to
/// [`assert_child_test_passed`]. A child still running at
/// [`CHILD_TEST_DEADLINE`] fails the caller with the host's load and
/// everything the child printed so far, so an overrun on a saturated host is
/// told apart from a hang at the point where it stopped.
pub fn run_child_test(
    command: &mut std::process::Command,
    test_name: &str,
    output_dir: &std::path::Path,
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
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("spawn child test `{test_name}`: {error}"));
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < CHILD_TEST_DEADLINE => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Ok(None) => break None,
            Err(error) => panic!("wait for child test `{test_name}`: {error}"),
        }
    };
    crate::process::bounded::kill_owned_group(child.id());
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let read = |path: &std::path::Path| std::fs::read(path).unwrap_or_default();
    let (stdout, stderr) = (read(&stdout_path), read(&stderr_path));
    let Some(status) = status else {
        panic!(
            "child test `{test_name}` was still running after {:?} ({}); killed. \
             Output so far:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            started.elapsed(),
            host_load(),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
    };
    crate::process::CapturedOutput {
        status,
        stdout,
        stderr,
    }
}

/// The host's load averages against its CPU count, for a fixture deadline's
/// failure message: an overrun on a saturated host then names the pressure
/// instead of reading as a defect in the code under test.
pub fn host_load() -> String {
    let cpus = std::thread::available_parallelism().map_or(0, std::num::NonZeroUsize::get);
    #[cfg(unix)]
    {
        let mut load = [0.0f64; 3];
        // Safety: `getloadavg` writes at most the three samples requested into
        // the array it is handed.
        if unsafe { libc::getloadavg(load.as_mut_ptr(), 3) } == 3 {
            return format!(
                "host load average {:.1} / {:.1} / {:.1} over 1/5/15 min on {cpus} CPUs",
                load[0], load[1], load[2]
            );
        }
    }
    format!("host load average unavailable; {cpus} CPUs")
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

/// Restores the variables captured by [`unset`] when dropped.
///
/// Holds a process-wide lock for its lifetime, so concurrent tests cannot
/// interleave environment mutations. Keep the guard's scope tight — in
/// particular, drop it before an `.await` (`clippy::await_holding_lock`); the
/// env is typically read during synchronous construction, so binding it around
/// just that call is enough.
#[must_use = "the environment is restored as soon as the guard is dropped"]
pub struct ScopedEnv {
    _lock: MutexGuard<'static, ()>,
    saved: Vec<(String, Option<String>)>,
}

/// Clear `names` for the returned guard's lifetime, restoring prior values on
/// drop. Names that are already unset are restored as unset.
pub fn unset<'a>(names: impl IntoIterator<Item = &'a str>) -> ScopedEnv {
    scoped(names.into_iter().map(|name| (name, None)))
}

/// Apply an explicit environment for the returned guard's lifetime, restoring
/// prior values on drop. `Some(value)` sets the variable, `None` clears it.
///
/// Use this instead of a bespoke set/restore pair whenever a test needs a
/// variable *present*: it captures the prior value the same way [`unset`] does
/// and takes the same process-wide lock, so a test that populates
/// `ORBIT_MANAGED_RUN_CONTEXT` cannot interleave with a sibling asserting the
/// unmanaged default (ORB-10540). A guard built here and a guard built by
/// [`unset`] are mutually exclusive; two independent locks would not be.
pub fn scoped<'a>(vars: impl IntoIterator<Item = (&'a str, Option<&'a str>)>) -> ScopedEnv {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let lock = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let requested = vars.into_iter().collect::<Vec<_>>();
    let saved = requested
        .iter()
        .map(|(name, _)| ((*name).to_string(), std::env::var(name).ok()))
        .collect::<Vec<_>>();
    // SAFETY: the guard holds the process-wide lock for the whole mutation
    // window, so no other guarded reader/writer runs concurrently.
    unsafe {
        for (name, value) in &requested {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
    ACTIVE_SCOPED_ENVS.fetch_add(1, Ordering::SeqCst);
    ScopedEnv { _lock: lock, saved }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        // SAFETY: the guard still holds the process-wide lock here.
        unsafe {
            for (name, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
        ACTIVE_SCOPED_ENVS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Pin `path` to `0o700`, making a fixture directory independent of the
/// ambient umask.
///
/// `tempfile::tempdir()` creates its root with `0o777 & !umask`. CI runs with
/// the conventional `umask 022`, so the root lands `0o755` and every
/// permission-sensitive check downstream happens to pass. A developer box with
/// a permissive umask (`002`, common with user-private groups, or `000`) gets
/// a group- or world-writable root instead. Permission-sensitive fixtures
/// need a deterministic private directory regardless of that ambient setting.
///
/// No-op on non-Unix targets.
#[cfg(unix)]
pub fn harden_dir(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::metadata(path)
        .unwrap_or_else(|error| panic!("read fixture dir metadata at {}: {error}", path.display()));
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(path, permissions)
        .unwrap_or_else(|error| panic!("chmod fixture dir {}: {error}", path.display()));
}

/// Non-Unix targets have no umask to defend against.
#[cfg(not(unix))]
pub fn harden_dir(_path: &std::path::Path) {}

/// Why this process cannot derive its own process-start identity token, when
/// it cannot. `None` means the probe works and a test may rely on it.
///
/// The token uses the UTC / C-locale `ps -o lstart=` rendering (see
/// [`crate::process::identity`]). Linux reads `/proc`; macOS tries libproc
/// before `ps`; other Unix hosts execute `ps`. When kernel data is unreadable
/// and no fallback is available, probes report
/// [`crate::process::identity::ProbeOutcome::Unavailable`]
/// and production takes its documented fail-safe branch — an owner it cannot
/// verify is neither finalized nor signalled. A test whose subject *is* the
/// derived token (TZ stability, recycled-PID detection, verified-owner
/// cancellation) has nothing to observe there. It names the constraint from
/// this helper and returns early, instead of failing for a reason unrelated to
/// the code under test. The message carries the probe error so the skip is
/// attributable from a log line.
pub fn start_identity_probe_blocker() -> Option<String> {
    crate::process::identity::self_start_identity_probe_blocker()
}

/// A `ps` run from a test: its output, or why the sandbox refused to start it.
#[derive(Debug)]
pub enum PsRun {
    /// `ps` started; its status and output are the caller's to assert.
    Ran(std::process::Output),
    /// An agent executor's sandbox refused to exec the setuid `ps`. The test
    /// has nothing to compare: it reports this reason as a skip and returns.
    Denied(String),
}

/// Run `ps -o lstart= -p <pid>` under the UTC / C locale that persisted owner
/// tokens were captured in.
///
/// Only `PermissionDenied` while starting `ps` yields [`PsRun::Denied`]; any
/// other failure to start it panics, and a `ps` that ran is returned whatever
/// its status, so a sandbox can never turn a wrong rendering into a pass.
pub fn ps_lstart_utc(pid: u32) -> PsRun {
    let mut command = std::process::Command::new("ps");
    command
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .env("LANG", "C");
    let spawned = match crate::process::run_bounded_capped_typed(
        &mut command,
        std::time::Duration::from_secs(10),
        64 * 1024,
    ) {
        Ok(captured) => Ok(std::process::Output {
            status: captured.status,
            stdout: captured.stdout,
            stderr: captured.stderr,
        }),
        Err(crate::process::BoundedRunError::Spawn(error)) => Err(error),
        Err(crate::process::BoundedRunError::Run(error)) => panic!("run ps: {error}"),
    };
    classify_ps(spawned)
}

pub(crate) fn classify_ps(spawned: std::io::Result<std::process::Output>) -> PsRun {
    match spawned {
        Ok(output) => PsRun::Ran(output),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            PsRun::Denied(format!("the sandbox denied running `ps`: {error}"))
        }
        Err(error) => panic!("run ps: {error}"),
    }
}

/// A live process that is no part of this one: not this process, its parent,
/// or its process group. Killed and reaped on drop.
#[cfg(unix)]
pub struct UnrelatedProcess(std::process::Child);

#[cfg(unix)]
impl UnrelatedProcess {
    /// The process id to bind or present as "some other process".
    pub fn pid(&self) -> u32 {
        self.0.id()
    }
}

#[cfg(unix)]
impl Drop for UnrelatedProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Spawn a sleeper in a process group of its own.
///
/// Tests that need "another live process" used pid 1, but macOS refuses an
/// unprivileged read of launchd's start time (and a sandboxed run refuses more
/// than that), so the binding the test wants to make never happens. A child
/// this process owns has a readable start time on every platform.
#[cfg(unix)]
pub fn spawn_unrelated_process() -> UnrelatedProcess {
    use std::os::unix::process::CommandExt;

    let mut command = std::process::Command::new("sleep");
    command.arg("600").process_group(0);
    let child = command
        .spawn()
        .unwrap_or_else(|error| panic!("spawn an unrelated process: {error}"));
    UnrelatedProcess(child)
}
