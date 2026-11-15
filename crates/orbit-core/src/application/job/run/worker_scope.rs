//! The read side of a pipeline worker's resource-containment scope
//! [ORB-12903], which run reconciliation reads to explain a worker that is
//! gone [ORB-17179].
//!
//! The run owns these reads, so `job/run` sits below `job/pipeline`. Pipeline
//! owns creating the scope, placing a worker in it and tearing it down, and
//! imports the shared read side from here: the scope's identity, how a
//! recorded scope is rebuilt, its limit counters and its systemd unit result.
//! Locating the scope of a live process and sampling its usage stay in
//! `pipeline/worker/scope.rs`.

use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Command;

/// Unit-name prefix of a worker scope, `orbit-worker-<run_id>-<nonce>.scope`.
pub(in crate::application::job) const SCOPE_UNIT_PREFIX: &str = "orbit-worker-";
/// Unit-name suffix of a worker scope.
pub(in crate::application::job) const SCOPE_UNIT_SUFFIX: &str = ".scope";

/// The cgroup of one worker scope, located from a process inside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkerScopeCgroup {
    unit: String,
    directory: PathBuf,
}

impl WorkerScopeCgroup {
    /// A scope the pipeline located from a process inside it: `directory` is
    /// the scope's cgroup directory and `unit` its worker-scope unit name.
    #[cfg(target_os = "linux")]
    pub(in crate::application::job) fn from_located(unit: String, directory: PathBuf) -> Self {
        Self { unit, directory }
    }

    /// Rebuild the scope a supervisor recorded for a worker it launched
    /// [ORB-17179]. Only a worker-scope unit whose cgroup directory sits under
    /// the cgroup v2 root and ends in that unit is accepted, so a recorded
    /// value can never point the counter reads anywhere else.
    #[cfg(target_os = "linux")]
    pub(crate) fn from_recorded(unit: &str, directory: &str) -> Option<Self> {
        let directory = PathBuf::from(directory);
        let contained = directory.starts_with("/sys/fs/cgroup")
            && directory.components().all(|component| {
                matches!(
                    component,
                    std::path::Component::RootDir | std::path::Component::Normal(_)
                )
            });
        (contained
            && unit.starts_with(SCOPE_UNIT_PREFIX)
            && unit.ends_with(SCOPE_UNIT_SUFFIX)
            && directory.file_name().is_some_and(|name| name == unit))
        .then(|| Self {
            unit: unit.to_string(),
            directory,
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn from_recorded(_unit: &str, _directory: &str) -> Option<Self> {
        None
    }

    /// The scope's systemd unit name.
    pub(crate) fn unit(&self) -> &str {
        &self.unit
    }

    /// The scope's cgroup directory.
    pub(crate) fn directory(&self) -> &std::path::Path {
        &self.directory
    }

    /// The scope's OOM-kill and refused-fork counters, zero or not. `None`
    /// when neither counter file can be read, as once systemd has removed
    /// the scope's cgroup.
    pub(crate) fn counters(&self) -> Option<ScopeCounters> {
        let read = |name: &str| std::fs::read_to_string(self.directory.join(name)).ok();
        let oom_kills = read("memory.events").map(|events| event_count(&events, "oom_kill"));
        let tasks_refused = read("pids.events").map(|events| event_count(&events, "max"));
        (oom_kills.is_some() || tasks_refused.is_some()).then_some(ScopeCounters {
            oom_kills,
            tasks_refused,
        })
    }

    /// The unit's state and result while the systemd user manager still
    /// holds it, as `state=<ActiveState>, result=<Result>`. `None` once the
    /// manager has collected the unit, or when it cannot be asked.
    #[cfg(target_os = "linux")]
    pub(crate) fn unit_result(&self) -> Option<String> {
        use orbit_common::process::bounded::run_bounded_capped_typed;
        use std::time::Duration;

        let mut command = Command::new("systemctl");
        command.args([
            "--user",
            "show",
            &self.unit,
            "--property=LoadState",
            "--property=ActiveState",
            "--property=Result",
        ]);
        let output = run_bounded_capped_typed(&mut command, Duration::from_secs(5), 4096).ok()?;
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let property = |key: &str| {
            stdout
                .lines()
                .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
                .map(str::trim)
                .filter(|value| !value.is_empty())
        };
        if property("LoadState") != Some("loaded") {
            return None;
        }
        Some(format!(
            "state={}, result={}",
            property("ActiveState").unwrap_or("unknown"),
            property("Result").unwrap_or("unknown"),
        ))
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn unit_result(&self) -> Option<String> {
        None
    }
}

/// A worker scope's limit counters as read, each `None` when its file was
/// unreadable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScopeCounters {
    /// `memory.events` `oom_kill`: processes the kernel OOM-killed in the scope.
    pub(crate) oom_kills: Option<u64>,
    /// `pids.events` `max`: forks refused at the scope's task limit.
    pub(crate) tasks_refused: Option<u64>,
}

/// The value of `key` in a cgroup flat-keyed file (`<key> <value>` lines).
pub(in crate::application::job) fn stat_value(content: &str, key: &str) -> Option<u64> {
    content
        .lines()
        .filter_map(|line| line.split_once(' '))
        .find(|(name, _)| *name == key)
        .and_then(|(_, value)| value.trim().parse().ok())
}

/// The value of `key` in a cgroup `*.events` file (`<key> <count>` lines).
pub(crate) fn event_count(events: &str, key: &str) -> u64 {
    stat_value(events, key).unwrap_or(0)
}
