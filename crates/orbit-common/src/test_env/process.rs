use super::FIXTURE_STEP_DEADLINE;

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
/// Run a freshly written fixture executable once, under
/// [`FIXTURE_STEP_DEADLINE`], before code with its own shorter timeout runs
/// it. Only the run matters, not its outcome.
///
/// macOS assesses each new executable on its first exec and holds the process
/// meanwhile, queued behind every other new image on the host. A substitute
/// `gh` written per fixture took longer to start on a loaded host than the
/// GitHub tool's production timeout, so a CI fixture read the CLI as
/// unavailable (ORB-15293). The assessment outlives this run, so the timed
/// call starts at once. Configure `command` the way the stub needs to exit
/// cleanly, without touching state the fixture asserts on.
pub fn prime_executable(command: &mut std::process::Command) {
    let started = std::time::Instant::now();
    let primed = crate::process::run_bounded_capped(command, FIXTURE_STEP_DEADLINE, 64 * 1024);
    assert!(
        primed.is_ok(),
        "priming {command:?} failed after {:.1?} ({}): {primed:?}",
        started.elapsed(),
        host_load()
    );
}

/// Poll `ready` until it holds.
///
/// Past [`FIXTURE_STEP_DEADLINE`] the fixture fails naming `what` it waited
/// for, how long, and the host load, so a wait that overran on a saturated
/// host is told apart from one that can never succeed.
pub fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let started = std::time::Instant::now();
    while !ready() {
        let waited = started.elapsed();
        assert!(
            waited < FIXTURE_STEP_DEADLINE,
            "timed out after {waited:.1?} waiting for {what} ({})",
            host_load()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
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
    super::classify_ps(spawned)
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
