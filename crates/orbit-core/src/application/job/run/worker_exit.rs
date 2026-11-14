//! The durable record of how a pipeline worker process ended, and the reads
//! stale-run reconciliation makes of it [ORB-17179].
//!
//! The run owns this evidence, so `job/run` sits below `job/pipeline`. This
//! module holds the persisted exit-record format (`pipeline.worker.exited`
//! and the claim row's scope fields) and its readers: the newest recorded
//! exit of a worker, whether it was a host-manager stop, and the clause
//! reconciliation appends when it interrupts a run whose worker is gone.
//! Pipeline's supervisor observes and writes the record, and imports the
//! format from here; see `pipeline/worker/exit.rs` for how a worker's exit is
//! observed, written and amended.
//!
//! Reconciliation that interrupts a run as `process_not_found` (or
//! `token_mismatch`) names the supervisor's exit row in its diagnostic. With
//! no row it says no exit status was observed, and reads what the worker's
//! scope, recorded when the worker claimed the run, still reports: its systemd
//! unit result and counters, or that the scope is gone. A terminating signal
//! that row attributes to the host manager (`systemd`), with no recorded
//! memory or task-limit breach, is a hold (`operator_stopped`) rather than
//! `worker_terminated`.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{AuditEventFilter, AuditEventStoreBackend};
use orbit_types::workflow::{JobRun, JobRunState};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::worker_scope::WorkerScopeCgroup;

/// The audit row a supervisor writes when it reaps its worker.
pub(crate) const WORKER_EXIT_AUDIT: &str = "pipeline.worker.exited";
/// Kernel `comm` of the user or system manager. `systemctl stop` and an
/// operator shutdown deliver the signal through that process. A direct
/// `kill` or `systemctl` comm, and `systemd-oomd`, are not this sender.
const HOST_MANAGER_SENDER: &str = "systemd";
/// The audit row a supervisor writes when its worker claims the run; it
/// carries the worker's scope.
pub(crate) const WORKER_CLAIM_AUDIT: &str = "pipeline.worker.claimed";

/// Reconciliation's clause for a worker nobody was seen reaping.
pub(in crate::application::job) const NO_EXIT_OBSERVED: &str =
    "no supervisor observed or recorded its exit status";

/// One reaped worker exit, as its supervisor observed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkerExit {
    pub(crate) worker_pid: u32,
    /// `ExitStatus` as the platform prints it.
    pub(crate) exit_status: String,
    #[serde(default)]
    pub(crate) exit_code: Option<i32>,
    #[serde(default)]
    pub(crate) signal: Option<i32>,
    #[serde(default)]
    pub(crate) signal_name: Option<String>,
    #[serde(default)]
    pub(crate) core_dumped: bool,
    #[serde(default)]
    pub(crate) scope: Option<ScopeEvidence>,
    /// Who sent the terminating `signal`, when the worker captured it.
    #[serde(default)]
    pub(crate) signal_sender: Option<SignalSender>,
    pub(crate) observed_at: DateTime<Utc>,
}

/// The process another process's terminating signal came from, as the
/// worker's handler read it from the kernel's `siginfo`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SignalSender {
    pub(crate) pid: u32,
    pub(crate) uid: u32,
    /// `/proc/<pid>/comm` while the handler ran; `None` once it was gone.
    #[serde(default)]
    pub(crate) command: Option<String>,
}

/// What a worker scope reported when it was read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ScopeEvidence {
    pub(crate) unit: String,
    #[serde(default)]
    pub(crate) unit_result: Option<String>,
    #[serde(default)]
    pub(crate) oom_kills: Option<u64>,
    #[serde(default)]
    pub(crate) tasks_refused: Option<u64>,
}

impl ScopeEvidence {
    /// Read `scope` now. The systemd unit result is asked only when
    /// `with_unit_result`: a supervisor reaping its worker would only see a
    /// unit that has not stopped yet.
    pub(crate) fn read(scope: &WorkerScopeCgroup, with_unit_result: bool) -> Self {
        let counters = scope.counters();
        Self {
            unit: scope.unit().to_string(),
            unit_result: with_unit_result.then(|| scope.unit_result()).flatten(),
            oom_kills: counters.and_then(|counters| counters.oom_kills),
            tasks_refused: counters.and_then(|counters| counters.tasks_refused),
        }
    }

    pub(crate) fn describe(&self) -> String {
        let mut facts = Vec::new();
        if let Some(result) = &self.unit_result {
            facts.push(format!("unit {result}"));
        }
        if let Some(count) = self.oom_kills {
            facts.push(format!("memory.events oom_kill={count}"));
        }
        if let Some(count) = self.tasks_refused {
            facts.push(format!("pids.events max={count}"));
        }
        if facts.is_empty() {
            return format!(
                "worker scope '{}' was already gone: no unit result or counters remain (see \
                 `journalctl --user -u {}`)",
                self.unit, self.unit
            );
        }
        format!("worker scope '{}': {}", self.unit, facts.join(", "))
    }
}

impl WorkerExit {
    /// Who sent a terminating signal, or that nobody is known to have:
    /// `None` for an exit no signal ended.
    pub(crate) fn sender_clause(&self) -> Option<String> {
        self.signal?;
        Some(match &self.signal_sender {
            Some(sender) => format!(
                "sent by pid {} (uid {}, command {})",
                sender.pid,
                sender.uid,
                sender.command.as_deref().map_or_else(
                    || "unreadable".to_string(),
                    |command| format!("'{command}'")
                ),
            ),
            None => "sender unknown: the worker captured none".to_string(),
        })
    }

    /// The operator-facing account of this exit.
    pub(crate) fn describe(&self) -> String {
        let how = match (self.signal, self.exit_code) {
            (Some(signal), _) => format!(
                "killed by signal {signal} ({}){}, {}",
                self.signal_name.as_deref().unwrap_or("unknown"),
                if self.core_dumped {
                    ", core dumped"
                } else {
                    ""
                },
                self.sender_clause().unwrap_or_default(),
            ),
            (None, Some(code)) => format!("exited with code {code}"),
            (None, None) => format!("exited with {}", self.exit_status),
        };
        let mut text = format!(
            "its supervisor reaped pid {} at {}: {how}",
            self.worker_pid,
            self.observed_at.to_rfc3339(),
        );
        if let Some(scope) = &self.scope {
            text.push_str("; ");
            text.push_str(&scope.describe());
        }
        text
    }

    /// True when a terminating signal was sent by the host manager.
    ///
    /// `systemd` is the kernel comm of the user or system manager, including
    /// an operator shutdown, a service stop, and a scope stop. A missing or
    /// unreadable sender, any other command (including `orbit`), an exit no
    /// signal ended, and a recorded memory or task-limit breach are not
    /// host-manager stops.
    pub(crate) fn stopped_by_host_manager(&self) -> bool {
        if self.signal.is_none() {
            return false;
        }
        if self.scope.as_ref().is_some_and(|scope| {
            scope.oom_kills.is_some_and(|count| count > 0)
                || scope.tasks_refused.is_some_and(|count| count > 0)
        }) {
            return false;
        }
        self.signal_sender
            .as_ref()
            .and_then(|sender| sender.command.as_deref())
            == Some(HOST_MANAGER_SENDER)
    }
}

/// Whether `run`'s recorded exit is a host-manager stop.
///
/// False when the run is not running, it has no worker pid, no matching exit
/// row exists, or the audit cannot be read. Callers that cannot prove the
/// stop keep the worker-terminated failure.
pub(crate) fn recorded_host_manager_stop(
    audit_events: &dyn AuditEventStoreBackend,
    run: &JobRun,
) -> bool {
    let Some(pid) = run.pid else {
        return false;
    };
    if run.state != JobRunState::Running {
        return false;
    }
    matches!(
        recorded_worker_exit(audit_events, &run.run_id, pid),
        Ok(Some(exit)) if exit.stopped_by_host_manager()
    )
}

/// The newest exit a supervisor recorded for `worker_pid` running `run_id`.
pub(crate) fn recorded_worker_exit(
    audit_events: &dyn AuditEventStoreBackend,
    run_id: &str,
    worker_pid: u32,
) -> Result<Option<WorkerExit>, OrbitError> {
    Ok(audit_arguments(audit_events, run_id, WORKER_EXIT_AUDIT)?
        .into_iter()
        .filter_map(|arguments| serde_json::from_value::<WorkerExit>(arguments).ok())
        .find(|exit| exit.worker_pid == worker_pid))
}

/// The scope recorded when `worker_pid` claimed `run_id`, if it ran in one.
fn recorded_worker_scope(
    audit_events: &dyn AuditEventStoreBackend,
    run_id: &str,
    worker_pid: u32,
) -> Result<Option<WorkerScopeCgroup>, OrbitError> {
    Ok(audit_arguments(audit_events, run_id, WORKER_CLAIM_AUDIT)?
        .into_iter()
        .find(|arguments| {
            arguments.get("worker_pid").and_then(Value::as_u64) == Some(u64::from(worker_pid))
        })
        .and_then(|arguments| {
            let unit = arguments.get("scope_unit")?.as_str()?;
            let directory = arguments.get("scope_cgroup")?.as_str()?;
            WorkerScopeCgroup::from_recorded(unit, directory)
        }))
}

/// Arguments of `run_id`'s `tool_name` audit rows, newest first.
fn audit_arguments(
    audit_events: &dyn AuditEventStoreBackend,
    run_id: &str,
    tool_name: &str,
) -> Result<Vec<Value>, OrbitError> {
    Ok(audit_events
        .list_audit_events(&AuditEventFilter {
            tool_name: Some(tool_name.to_string()),
            job_run_id: Some(run_id.to_string()),
            limit: 20,
            ..AuditEventFilter::default()
        })?
        .into_iter()
        .filter_map(|row| serde_json::from_str(row.arguments_json.as_deref()?).ok())
        .collect())
}

/// The clause reconciliation appends when it interrupts `run` because its
/// recorded worker is gone: the exit its supervisor recorded, or that none
/// was observed and what the worker's scope still reports. `None` for a run
/// that never recorded a worker.
pub(crate) fn reconciled_worker_exit_clause(
    audit_events: &dyn AuditEventStoreBackend,
    run: &JobRun,
) -> Option<String> {
    let pid = run.pid?;
    let clause = match recorded_worker_exit(audit_events, &run.run_id, pid) {
        Ok(Some(exit)) => exit.describe(),
        Ok(None) => {
            let scope = match recorded_worker_scope(audit_events, &run.run_id, pid) {
                Ok(Some(scope)) => ScopeEvidence::read(&scope, true).describe(),
                Ok(None) => "no worker scope evidence is available".to_string(),
                Err(error) => format!("worker scope evidence unreadable: {error}"),
            };
            format!("{NO_EXIT_OBSERVED}; {scope}")
        }
        Err(error) => format!("{NO_EXIT_OBSERVED}: exit records unreadable: {error}"),
    };
    Some(format!("worker exit: {clause}"))
}
