//! Resource containment for detached pipeline workers [ORB-12903].
//!
//! A worker and everything it spawns (agent CLIs, cargo, rustc, test binaries)
//! otherwise inherit the cgroup of whichever service launched it, so nothing
//! stands between one runaway run and the host's global OOM killer (2026-09-23
//! outage). On Linux with a reachable systemd user manager each worker instead
//! runs in its own transient scope, `orbit-worker-<run_id>-<nonce>.scope`,
//! created by `systemd-run --user --scope` with the global `machine.worker_*`
//! limits. `systemd-run --scope` registers its own PID in the new unit and then
//! execs the worker in place, so the child the supervisor spawned, reaps and
//! signals is still the worker itself: same PID, same `setsid` session.
//!
//! The scope sets `OOMPolicy=continue`: the kernel OOM-kills the largest
//! process inside the run rather than systemd stopping the whole unit, so the
//! worker usually survives to report the breach on its own run. Either side
//! reads the scope's `memory.events` / `pids.events` counters to tell a
//! resource-limit failure apart from any other one.
//!
//! Where containment is disabled or unavailable (macOS, containers, sandboxes
//! without a user bus) workers launch exactly as before and one warning per
//! process says why.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Once;

use orbit_config::{MemoryLimit, WorkerContainmentSettings};

/// Error code on the diagnostic step of a run that failed after its worker
/// scope hit a memory or task limit.
pub(crate) const WORKER_RESOURCE_LIMIT_ERROR_CODE: &str = "worker_resource_limit";

/// Unit-name prefix of every worker scope. Breach detection only trusts
/// counters from a cgroup carrying it, so a worker that fell back to its
/// parent's service cgroup never blames sibling work on its own run.
const SCOPE_UNIT_PREFIX: &str = "orbit-worker-";
const SCOPE_UNIT_SUFFIX: &str = ".scope";
const SYSTEMD_RUN: &str = "systemd-run";

/// The limits applied to one worker scope, admitted from `machine.worker_*`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkerLimits {
    memory_high: MemoryLimit,
    memory_max: MemoryLimit,
    tasks_max: u32,
}

impl WorkerLimits {
    /// `None` only when containment is disabled: the memory limits were
    /// typed at config admission, so enabled settings always yield limits.
    pub(crate) fn from_settings(settings: &WorkerContainmentSettings) -> Option<Self> {
        settings.enabled.then_some(Self {
            memory_high: settings.memory_high,
            memory_max: settings.memory_max,
            tasks_max: settings.tasks_max,
        })
    }

    /// The unit properties, in `systemd-run --property=` form.
    fn properties(&self) -> [String; 4] {
        [
            format!("MemoryHigh={}", self.memory_high),
            format!("MemoryMax={}", self.memory_max),
            format!("TasksMax={}", self.tasks_max),
            "OOMPolicy=continue".to_string(),
        ]
    }
}

/// Launch `base` inside a fresh worker scope when containment is configured
/// and the user manager accepts transient scopes; otherwise return `base`
/// unchanged after a once-per-process warning.
pub(crate) fn contain_worker_command(
    base: Command,
    run_id: &str,
    limits: Option<&WorkerLimits>,
) -> Command {
    let Some(limits) = limits else {
        warn_uncontained("machine.worker_containment is false");
        return base;
    };
    match user_scope_availability() {
        Ok(()) => scoped_worker_command(&base, &scope_unit_name(run_id), limits),
        Err(reason) => {
            warn_uncontained(&reason);
            base
        }
    }
}

fn warn_uncontained(reason: &str) {
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        tracing::warn!(
            target: "orbit.core.job_run",
            reason,
            "pipeline workers launch without a bounded systemd scope; a runaway run shares \
             its launcher's cgroup and memory",
        );
    });
}

/// A unit name unique to this launch. The nonce keeps a duplicate delivery of
/// the same run from colliding with the incumbent's scope.
pub(crate) fn scope_unit_name(run_id: &str) -> String {
    let run = run_id
        .chars()
        .take(128)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let nonce = getrandom::u32().unwrap_or_else(|_| std::process::id());
    format!("{SCOPE_UNIT_PREFIX}{run}-{nonce:08x}{SCOPE_UNIT_SUFFIX}")
}

/// `systemd-run --user --scope … -- <base argv>`, carrying over `base`'s
/// working directory and environment edits. `--scope` execs the program in
/// the same process, so stdio and `pre_exec` set on the result later still
/// apply to the worker.
pub(crate) fn scoped_worker_command(base: &Command, unit: &str, limits: &WorkerLimits) -> Command {
    let mut command = Command::new(SYSTEMD_RUN);
    command
        .args(["--user", "--scope", "--quiet", "--collect"])
        .arg(format!("--unit={unit}"));
    for property in limits.properties() {
        command.arg(format!("--property={property}"));
    }
    command
        .arg("--")
        .arg(base.get_program())
        .args(base.get_args());
    if let Some(directory) = base.get_current_dir() {
        command.current_dir(directory);
    }
    for (key, value) in base.get_envs() {
        match value {
            Some(value) => command.env(key, value),
            None => command.env_remove(key),
        };
    }
    command.stdin(Stdio::null());
    command
}

/// Whether this process can create transient scopes in a user manager, probed
/// once per process with the same properties a worker scope uses.
#[cfg(target_os = "linux")]
fn user_scope_availability() -> Result<(), String> {
    use std::sync::OnceLock;

    static PROBE: OnceLock<Result<(), String>> = OnceLock::new();
    PROBE.get_or_init(probe_user_scope).clone()
}

#[cfg(not(target_os = "linux"))]
fn user_scope_availability() -> Result<(), String> {
    Err("worker scopes need Linux with a systemd user manager".to_string())
}

#[cfg(target_os = "linux")]
fn probe_user_scope() -> Result<(), String> {
    use std::io::Read;
    use std::time::{Duration, Instant};

    const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

    // Every property a worker scope sets, so a manager too old for one of
    // them (`OOMPolicy=` on scopes) falls back here rather than failing each
    // worker launch.
    let mut child = Command::new(SYSTEMD_RUN)
        .args(["--user", "--scope", "--quiet", "--collect"])
        .args([
            "--property=MemoryHigh=infinity",
            "--property=MemoryMax=infinity",
            "--property=TasksMax=infinity",
            "--property=OOMPolicy=continue",
        ])
        .args(["--", "true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cannot run {SYSTEMD_RUN}: {error}"))?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{SYSTEMD_RUN} --user --scope did not answer within {}s",
                    PROBE_TIMEOUT.as_secs()
                ));
            }
            Err(error) => return Err(format!("wait for {SYSTEMD_RUN}: {error}")),
        }
    };
    if status.success() {
        return Ok(());
    }
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    let detail = stderr.lines().next().unwrap_or_default().trim();
    Err(format!(
        "{SYSTEMD_RUN} --user --scope exited with {status}: {detail}"
    ))
}

/// The cgroup of one worker scope, located from a process inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkerScopeCgroup {
    unit: String,
    directory: PathBuf,
}

/// A worker scope's recorded limit hits. Only built when at least one counter
/// is non-zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ScopeLimitBreach {
    unit: String,
    oom_kills: u64,
    tasks_refused: u64,
    memory_max: Option<String>,
    tasks_max: Option<String>,
}

impl WorkerScopeCgroup {
    /// The worker scope `pid` runs in, if it runs in one.
    pub(crate) fn of_process(pid: u32) -> Option<Self> {
        Self::read(&format!("/proc/{pid}/cgroup"))
    }

    /// The scope's cgroup directory.
    #[cfg(test)]
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    /// The worker scope this process runs in, if it runs in one.
    pub(crate) fn of_current_process() -> Option<Self> {
        Self::read("/proc/self/cgroup")
    }

    #[cfg(target_os = "linux")]
    fn read(proc_cgroup: &str) -> Option<Self> {
        let content = std::fs::read_to_string(proc_cgroup).ok()?;
        Self::from_proc_cgroup(&content, Path::new("/sys/fs/cgroup"))
    }

    #[cfg(not(target_os = "linux"))]
    fn read(_proc_cgroup: &str) -> Option<Self> {
        None
    }

    /// Parse `/proc/<pid>/cgroup` (cgroup v2 `0::<path>` line) and keep it
    /// only when its leaf is a worker scope.
    ///
    /// Its only production caller is [`Self::read`]'s Linux arm, but unit
    /// tests exercise it directly on every platform the suite runs on.
    #[cfg(any(target_os = "linux", test))]
    pub(crate) fn from_proc_cgroup(content: &str, cgroup_root: &Path) -> Option<Self> {
        let path = content.lines().find_map(|line| line.strip_prefix("0::"))?;
        let unit = path.rsplit('/').next()?;
        if !(unit.starts_with(SCOPE_UNIT_PREFIX) && unit.ends_with(SCOPE_UNIT_SUFFIX)) {
            return None;
        }
        Some(Self {
            unit: unit.to_string(),
            directory: cgroup_root.join(path.trim_start_matches('/')),
        })
    }

    /// The scope's limit hits so far, or `None` when it hit none (or its
    /// cgroup is already gone).
    pub(crate) fn limit_breach(&self) -> Option<ScopeLimitBreach> {
        let read = |name: &str| std::fs::read_to_string(self.directory.join(name)).ok();
        let oom_kills = read("memory.events").map_or(0, |events| event_count(&events, "oom_kill"));
        let tasks_refused = read("pids.events").map_or(0, |events| event_count(&events, "max"));
        if oom_kills == 0 && tasks_refused == 0 {
            return None;
        }
        let limit = |name: &str| read(name).map(|value| value.trim().to_string());
        Some(ScopeLimitBreach {
            unit: self.unit.clone(),
            oom_kills,
            tasks_refused,
            memory_max: limit("memory.max"),
            tasks_max: limit("pids.max"),
        })
    }
}

impl ScopeLimitBreach {
    /// The operator-facing cause, naming the limit and how often it was hit.
    pub(crate) fn describe(&self) -> String {
        let mut hits = Vec::new();
        if self.oom_kills > 0 {
            hits.push(format!(
                "the kernel OOM-killed {} process(es) at its memory limit (memory.max={})",
                self.oom_kills,
                self.memory_max.as_deref().unwrap_or("unknown"),
            ));
        }
        if self.tasks_refused > 0 {
            hits.push(format!(
                "{} fork/clone attempt(s) were refused at its task limit (pids.max={})",
                self.tasks_refused,
                self.tasks_max.as_deref().unwrap_or("unknown"),
            ));
        }
        format!(
            "worker scope '{}' hit its resource limits: {}; raise machine.worker_memory_max / \
             machine.worker_tasks_max only if the run's workload legitimately needs more",
            self.unit,
            hits.join("; "),
        )
    }
}

/// The value of `key` in a cgroup `*.events` file (`<key> <count>` lines).
pub(crate) fn event_count(events: &str, key: &str) -> u64 {
    events
        .lines()
        .filter_map(|line| line.split_once(' '))
        .find(|(name, _)| *name == key)
        .and_then(|(_, count)| count.trim().parse().ok())
        .unwrap_or(0)
}
