//! Stale, superseded, task-outcome and failed-partition reports.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::automation::members::PreparationPolicy;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::contracts::SUPERSEDED_BY_SOURCE;
use crate::application::task::TaskAutomationPort;

use super::super::apply::admission::PreparedTaskSnapshot;
use super::apply_task::{CONTEXT_CREATION_CHANGED, CONTEXT_CREATION_CHANGED_DETAIL};
use super::drift::task_snapshot_drift;
use super::history::{material_change_detail, status_only_fingerprint};

pub(in super::super) fn stale_task(task_id: &str, reason: &str, detail: &str) -> Value {
    json!({
        "task_id": task_id,
        "outcome": "stale",
        "reason": reason,
        "detail": detail,
    })
}

/// A newer owner can make a prepared assessment unnecessary without making
/// the pilot fail. Durable task edits still refuse the stale write, but settle
/// it as superseded, as do admission, execution claims and terminal decisions.
pub(in super::super) fn superseded_task(
    runtime: &OrbitRuntime,
    task_id: &str,
    snapshot: &PreparedTaskSnapshot,
    policy: &PreparationPolicy,
    ci_sweep: bool,
) -> Result<Option<Value>, OrbitError> {
    if !matches!(snapshot.status, TaskStatus::Proposed | TaskStatus::Backlog) {
        return Ok(None);
    }
    let current = match runtime.get_task(task_id) {
        Ok(task) => task,
        Err(OrbitError::NotFound { .. }) => return Ok(None),
        Err(error) => return Err(error),
    };
    let superseded = |reason: &str, detail: &str| {
        super::super::superseded_outcome(task_id, reason, current.status, detail)
    };
    if ci_sweep && matches!(current.status, TaskStatus::Rejected | TaskStatus::Archived) {
        return Ok(Some(superseded(
            "operator_rejected",
            "the operator rejected or archived the CI-sweep task before admission",
        )));
    }
    if ci_sweep && current.status != snapshot.status {
        // Other status changes continue through admission: a promotion
        // elsewhere settles superseded there unless this operation's receipt
        // proves its own promotion already landed, and any other status fails
        // its proposed-status check.
        return Ok(None);
    }
    if !ci_sweep
        && matches!(
            current.status,
            TaskStatus::Done | TaskStatus::Rejected | TaskStatus::Archived
        )
    {
        return Ok(Some(superseded(
            "task_terminal",
            "task became terminal after preparation",
        )));
    }
    if matches!(current.status, TaskStatus::InProgress | TaskStatus::Review)
        && runtime
            .inspect_execution_claims()?
            .iter()
            .any(|inspection| {
                inspection.claim.task_id == task_id && inspection.claim.phase.protects_footprint()
            })
    {
        return Ok(Some(superseded(
            "execution_claim",
            "an execution claim superseded preparation",
        )));
    }
    let history = runtime.get_task_history(task_id)?;
    let newer = &history[snapshot.history_len.unwrap_or(0).min(history.len())..];
    let edited = snapshot.history_len.is_some()
        && newer.iter().any(|entry| {
            // The pilot's own atomic mutation can re-seal a creation grant.
            // Its replay receipt remains authoritative for that same write.
            entry.by != "task-pilot"
                && matches!(
                    entry.event.as_str(),
                    "updated"
                        | "renamed"
                        | "crew_assigned"
                        | "crew_redrawn"
                        | "context_creation_authorized"
                )
        });
    let structural_drift = current.context_files != snapshot.context_files
        || current.complexity != snapshot.complexity
        || current.title != snapshot.title
        || current.tags != snapshot.tags;
    let creation_drift =
        runtime.context_creation_state(&current)?.identity() != snapshot.context_creation_identity;
    if edited {
        let material_drift = if let Some((_, revision)) = &snapshot.material {
            let fresh = runtime
                .task_pilot_fingerprints(&current, revision, policy)
                .map_err(orbit_automation::automation_error_to_orbit)?;
            snapshot
                .status_neutral_fingerprint
                .as_ref()
                .is_some_and(|expected| expected != &fresh.status_neutral)
        } else {
            // No Git material hash: an actual field update after the captured
            // history boundary is sufficient to withhold this old assessment.
            true
        };
        if structural_drift || material_drift {
            let (reason, detail) = task_snapshot_drift(runtime, &current, snapshot, policy)
                .unwrap_or(("task_edited", "task fields changed after preparation"));
            let detail = if reason == "material_changed" {
                material_change_detail(runtime, &current, snapshot, policy)
            } else {
                detail.to_string()
            };
            return Ok(Some(superseded(reason, &detail)));
        }
        if creation_drift {
            return Ok(Some(superseded(
                CONTEXT_CREATION_CHANGED,
                CONTEXT_CREATION_CHANGED_DETAIL,
            )));
        }
    }
    if snapshot.history_len.is_some()
        && !matches!(current.status, TaskStatus::Proposed | TaskStatus::Backlog)
        && newer
            .iter()
            .any(|entry| entry.to_status == Some(current.status))
    {
        return Ok(Some(superseded(
            "status_changed",
            "a durable task transition superseded preparation",
        )));
    }
    if matches!(current.status, TaskStatus::InProgress | TaskStatus::Review)
        && !structural_drift
        && !creation_drift
        && newer.iter().any(|entry| {
            entry.to_status == Some(TaskStatus::InProgress)
                && entry.by == "system"
                && entry
                    .note
                    .as_deref()
                    .is_some_and(|note| note.starts_with("workflow admission:"))
        })
        && (status_only_fingerprint(runtime, &current, snapshot, policy).is_some()
            || (snapshot.material.is_none() && snapshot.history_len.is_some() && !edited))
    {
        return Ok(Some(superseded(
            "workflow_admission",
            "workflow admission superseded preparation",
        )));
    }
    Ok(None)
}

pub(in super::super) fn task_outcome(task_id: &str, outcome: &str, error: Option<String>) -> Value {
    json!({ "task_id": task_id, "outcome": outcome, "error": error })
}

/// The branch changed this task's material after its claim froze the
/// source: the assessment is not applied, and the member is claimed afresh
/// at the head rather than retried or retired [ORB-14476].
pub(in super::super) fn source_superseded(task_id: &str, detail: &str) -> Value {
    json!({
        "task_id": task_id,
        "outcome": "superseded",
        "reason": SUPERSEDED_BY_SOURCE,
        "detail": detail,
    })
}

/// A partition whose result is unusable fails its tasks as invalid, except
/// those `superseded` by a source move, which settle superseded instead.
pub(in super::super) fn failed_partition(
    partition_index: u64,
    task_ids: &[String],
    error: String,
    superseded: &BTreeMap<String, String>,
) -> Value {
    let task_outcomes = task_ids
        .iter()
        .map(|task_id| match superseded.get(task_id) {
            Some(detail) => source_superseded(task_id, detail),
            None => task_outcome(task_id, "invalid", Some(error.clone())),
        })
        .collect::<Vec<_>>();
    let unresolved = task_ids
        .iter()
        .filter(|task_id| !superseded.contains_key(*task_id))
        .count();
    json!({
        "partition_index": partition_index,
        "task_ids": task_ids,
        "outcome": if unresolved == 0 { "superseded" } else { "failed" },
        "error": (unresolved > 0).then_some(error),
        "task_outcomes": task_outcomes,
        "unresolved_count": unresolved,
        "applied_task_ids": [],
    })
}
