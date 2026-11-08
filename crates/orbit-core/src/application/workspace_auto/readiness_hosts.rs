//! Follower capability observations for owner readiness, never admission authority.
use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{
    EXECUTOR_CENSUS_LIVENESS_SECONDS, ExecutorCensusEntry, browser_kit_unavailable,
    labels_unavailable, os_unavailable,
};
use orbit_types::task::{ExecutorOsLimit, Task};
use serde::Serialize;
use serde_json::json;

use crate::OrbitRuntime;

/// Read once per readiness response. Historical claims and host reachability
/// cannot establish the capability of a currently pulling drain.
pub(super) struct FollowerEvidence {
    rows: Result<Vec<(ExecutorCensusEntry, FollowerReportState)>, OrbitError>,
    limits: Result<BTreeMap<String, ExecutorOsLimit>, OrbitError>,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum FollowerReportState {
    Stopped,
    Unavailable,
    Stale,
    Recent,
}

impl FollowerReportState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Unavailable => "unavailable",
            Self::Stale => "stale",
            Self::Recent => "recent",
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum FollowerEvidenceStatus {
    Unavailable,
    NoCompatibleRecent,
    CompatibleRecent,
}

#[derive(Serialize)]
struct FollowerExecutorObservation {
    machine_id: String,
    run_id: String,
    seen_at: DateTime<Utc>,
    state: FollowerReportState,
    compatible: bool,
    refusal: Option<String>,
}

#[derive(Serialize)]
pub(super) struct FollowerExecutorEvidence {
    pub(super) status: FollowerEvidenceStatus,
    executors: Vec<FollowerExecutorObservation>,
    detail: String,
}

impl FollowerEvidence {
    pub(super) fn read(runtime: &OrbitRuntime) -> Self {
        let now = Utc::now();
        Self {
            rows: runtime
                .stores()
                .tasks()
                .recorded_executor_census()
                .map(|rows| {
                    rows.into_iter()
                        .filter(|row| {
                            Some(row.machine_id.as_str()) != runtime.automation_machine_identity()
                        })
                        .map(|row| {
                            let state = follower_report_state(runtime, &row, now);
                            (row, state)
                        })
                        .collect()
                }),
            limits: runtime.executor_host_os_limits(),
        }
    }

    pub(super) fn task_follower_evidence(&self, task: &Task) -> FollowerExecutorEvidence {
        let (rows, limits) = match (&self.rows, &self.limits) {
            (Ok(rows), Ok(limits)) => (rows, limits),
            _ => {
                return FollowerExecutorEvidence {
                    status: FollowerEvidenceStatus::Unavailable,
                    executors: Vec::new(),
                    detail: "Follower executor evidence unavailable: capability declarations or executor OS limits could not be read. This host's OS refusal is local; follower admission is unknown.".to_string(),
                };
            }
        };
        let executors = rows
            .iter()
            .map(|(row, state)| {
                let refusal = capability_refusal(row, task, limits);
                FollowerExecutorObservation {
                    machine_id: row.machine_id.clone(),
                    run_id: row.run_id.clone(),
                    seen_at: row.seen_at,
                    state: *state,
                    compatible: refusal.is_none(),
                    refusal,
                }
            })
            .collect::<Vec<_>>();
        let compatible = executors
            .iter()
            .filter(|row| row.compatible && row.state == FollowerReportState::Recent)
            .map(|row| {
                format!(
                    "{} (run {}, last reported {})",
                    row.machine_id,
                    row.run_id,
                    // Match Chrono's serde spelling, including `Z` and
                    // automatic fractional-second precision.
                    row.seen_at.to_rfc3339_opts(SecondsFormat::AutoSi, true)
                )
            })
            .collect::<Vec<_>>();
        let (status, detail) = if compatible.is_empty() {
            let observations = executors
                .iter()
                .map(|row| {
                    format!(
                        "{} (run {}): {}{}",
                        row.machine_id,
                        row.run_id,
                        row.state.as_str(),
                        row.refusal
                            .as_deref()
                            .map(|reason| format!(", {reason}"))
                            .unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>();
            (
                FollowerEvidenceStatus::NoCompatibleRecent,
                format!(
                    "No recent compatible executor declaration is available{}. Follower admission is unknown; registration and past claims do not prove a running eligible drain.",
                    if observations.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", observations.join("; "))
                    }
                ),
            )
        } else {
            (
                FollowerEvidenceStatus::CompatibleRecent,
                format!(
                    "Compatible executor declaration(s): {}. Reports expire after {} seconds without another pull. A recent report is not a live heartbeat; follower capacity and admission remain unverified.",
                    compatible.join("; "),
                    EXECUTOR_CENSUS_LIVENESS_SECONDS
                ),
            )
        };
        FollowerExecutorEvidence {
            status,
            executors,
            detail,
        }
    }
}

fn follower_report_state(
    runtime: &OrbitRuntime,
    row: &ExecutorCensusEntry,
    now: DateTime<Utc>,
) -> FollowerReportState {
    // Run IDs alone are not a cross-machine identity: only matching location
    // evidence can establish that a locally visible run is this follower's.
    match runtime.stores().jobs().get_job_run(&row.run_id) {
        Ok(Some(run))
            if run.state.is_terminal()
                && run
                    .executed_on
                    .as_ref()
                    .is_some_and(|location| location.machine_id == row.machine_id) =>
        {
            FollowerReportState::Stopped
        }
        Err(_) => FollowerReportState::Unavailable,
        _ if !row.fresh(now) || row.seen_at > now => FollowerReportState::Stale,
        _ => FollowerReportState::Recent,
    }
}

fn capability_refusal(
    row: &ExecutorCensusEntry,
    task: &Task,
    limits: &BTreeMap<String, ExecutorOsLimit>,
) -> Option<String> {
    let capability = &row.capability;
    os_unavailable(capability.os, task, limits)
        .or_else(|| browser_kit_unavailable(capability.browser_kit, task))
        .or_else(|| labels_unavailable(capability.os, &capability.labels, task))
        .or_else(|| {
            capability
                .crews
                .as_ref()
                .and_then(|crews| crews.unrunnable_reason(task.crew.as_deref()))
        })
}

impl OrbitRuntime {
    /// Observation keys for changes to follower capabilities, drain identity,
    /// local stop evidence or freshness. Used by the dashboard change stream;
    /// reads no remote host and changes neither census nor admission state.
    pub fn executor_readiness_change_keys(&self) -> Result<Vec<String>, OrbitError> {
        let evidence = FollowerEvidence::read(self);
        let rows = evidence
            .rows
            .as_ref()
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        Ok(rows
            .iter()
            .map(|(row, state)| {
                json!({"machine": row.machine_id, "run": row.run_id,
            "capability": row.capability, "state": state})
                .to_string()
            })
            .collect())
    }
}

/// Use the same normalized selectors and lock modes as local admission, with
/// no wave selection, capacity guess or mutation of the admission snapshot.
pub(super) fn follower_footprint_conflicts(
    runtime: &OrbitRuntime,
    task: &Task,
    holders: &crate::application::admission::auto_admission::AdmissionHolders,
) -> Vec<crate::application::admission::auto_admission::AdmissionConflict> {
    let mut conflicts = crate::runtime::task::locks::lock_context_files_for_task(
        task,
        &runtime.paths().repo_root,
        runtime.unlocked_context_paths(),
    )
    .iter()
    .flat_map(|selector| holders.footprint_conflicts_for_selector(selector))
    .filter(|conflict| conflict.blocking_task_id != task.id)
    .collect::<Vec<_>>();
    conflicts.sort();
    conflicts.dedup();
    conflicts
}
