//! What is durably known about how a pipeline worker process ended
//! [ORB-17179].
//!
//! Only the worker's parent, the supervisor that launched it, can reap its
//! exit status. On Linux an unreaped worker keeps its start identity, so
//! liveness reconciliation cannot condemn a worker until that supervisor has
//! reaped it. Once it has, the two race to settle the run. When the
//! supervisor is gone, init or the user manager reaps the worker and its
//! status is lost. So:
//!
//! - The supervisor records a `pipeline.worker.exited` audit row the moment
//!   it reaps, before it settles the run: exit code or terminating signal,
//!   and the worker scope's OOM-kill and refused-fork counters while the
//!   scope's cgroup still exists.
//! - Reconciliation that interrupts a run as `process_not_found` (or
//!   `token_mismatch`) names that row in its diagnostic. With no row it says
//!   no exit status was observed, and reads what the worker's scope, recorded
//!   when the worker claimed the run, still reports: its systemd unit result
//!   and counters, or that the scope is gone.
//! - A supervisor that reaps after reconciliation already wrote "no exit
//!   status was observed" appends what it observed to that diagnostic.

use std::process::ExitStatus;
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{
    AuditEventFilter, AuditEventStoreBackend, JobRunStepParams, JobRunStoreBackend,
};
use orbit_types::workflow::{JobRun, JobRunState};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::scope::WorkerScopeCgroup;
use crate::application::job::run::WORKER_TERMINATED_ERROR_CODE;

/// The audit row a supervisor writes when it reaps its worker.
pub(crate) const WORKER_EXIT_AUDIT: &str = "pipeline.worker.exited";
/// The audit row a supervisor writes when its worker claims the run; it
/// carries the worker's scope.
pub(crate) const WORKER_CLAIM_AUDIT: &str = "pipeline.worker.claimed";

/// Reconciliation's clause for a worker nobody was seen reaping.
const NO_EXIT_OBSERVED: &str = "no supervisor observed or recorded its exit status";
/// Marks a diagnostic a supervisor that reaped after reconciliation amended.
const OBSERVED_AFTERWARDS: &str = "afterwards its supervisor reaped";
/// How long a supervisor that lost the race waits for reconciliation's
/// diagnostic step, which is written just after the interrupted state.
const RECONCILED_STEP_WAIT: Duration = Duration::from_secs(5);

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
    pub(crate) observed_at: DateTime<Utc>,
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
    /// The exit `status` of `worker_pid`, with what its scope reports now.
    pub(crate) fn observe(
        worker_pid: u32,
        status: &ExitStatus,
        scope: Option<&WorkerScopeCgroup>,
    ) -> Self {
        #[cfg(unix)]
        let (signal, core_dumped) = {
            use std::os::unix::process::ExitStatusExt;
            (status.signal(), status.core_dumped())
        };
        #[cfg(not(unix))]
        let (signal, core_dumped) = (None, false);
        Self {
            worker_pid,
            exit_status: status.to_string(),
            exit_code: status.code(),
            signal,
            signal_name: signal.and_then(signal_name).map(str::to_string),
            core_dumped,
            scope: scope.map(|scope| ScopeEvidence::read(scope, false)),
            observed_at: Utc::now(),
        }
    }

    /// The operator-facing account of this exit.
    pub(crate) fn describe(&self) -> String {
        let how = match (self.signal, self.exit_code) {
            (Some(signal), _) => format!(
                "killed by signal {signal} ({}){}",
                self.signal_name.as_deref().unwrap_or("unknown"),
                if self.core_dumped {
                    ", core dumped"
                } else {
                    ""
                },
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

/// Append `exit` to the diagnostic reconciliation wrote when it interrupted
/// `run_id` before this supervisor reaped `exit.worker_pid`. A run another
/// path settled, or a diagnostic that already names an exit, is left alone.
/// Reconciliation writes its step just after the interrupted state, so a
/// missing step is waited for briefly.
pub(crate) fn note_exit_after_reconciliation(
    runs: &dyn JobRunStoreBackend,
    run_id: &str,
    exit: &WorkerExit,
) -> Result<(), OrbitError> {
    let deadline = Instant::now() + RECONCILED_STEP_WAIT;
    loop {
        let Some(run) = runs.get_job_run(run_id)? else {
            return Ok(());
        };
        if run.state != JobRunState::Interrupted || run.pid != Some(exit.worker_pid) {
            return Ok(());
        }
        let error_step = run
            .steps
            .iter()
            .find(|step| step.error_code.is_some() || step.error_message.is_some());
        let Some(step) = error_step else {
            if Instant::now() >= deadline {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
            continue;
        };
        let Some(message) = step.error_message.as_deref() else {
            return Ok(());
        };
        if step.error_code.as_deref() != Some(WORKER_TERMINATED_ERROR_CODE)
            || !message.contains(NO_EXIT_OBSERVED)
            || message.contains(OBSERVED_AFTERWARDS)
        {
            return Ok(());
        }
        let Some(started_at) = step.started_at else {
            return Ok(());
        };
        let finished_at = step.finished_at.unwrap_or(started_at);
        let amended = format!("{message}; afterwards {}", exit.describe());
        runs.complete_job_run_step(
            run_id,
            &JobRunStepParams {
                lock_wait_ms: step.lock_wait_ms,
                step_index: step.step_index as usize,
                target_type: step.target_type,
                target_id: step.target_id.clone(),
                started_at,
                finished_at,
                duration_ms: step.duration_ms,
                exit_code: step.exit_code,
                agent_response_json: step.agent_response_json.clone(),
                state: step.state,
                error_code: step.error_code.clone(),
                error_message: Some(amended),
            },
        )?;
        return Ok(());
    }
}

/// The conventional name of a terminating signal.
#[cfg(unix)]
fn signal_name(signal: i32) -> Option<&'static str> {
    Some(match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        libc::SIGSYS => "SIGSYS",
        _ => return None,
    })
}

#[cfg(not(unix))]
fn signal_name(_signal: i32) -> Option<&'static str> {
    None
}
