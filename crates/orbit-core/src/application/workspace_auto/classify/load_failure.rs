//! Tasks this drain must not re-admit after its own leaf failed at job load.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_store::V2AuditEventFilter;
use orbit_types::workflow::{JobRunState, JobTargetType};

use crate::OrbitRuntime;
use crate::application::admission::pass_error::PassError;

use super::super::drains::LEAF_JOB_NAME;

/// Audit envelope of an activity that has started. The run projection turns
/// this event into an activity step; failure finalization does not copy that
/// step onto the job record.
const STARTED_STEP_EVENT_TYPE: &str = "step.started";
const STARTED_STEP_BODY_KIND: &str = "step_started";

/// One task held for the rest of this drain after its leaf failed at load.
#[derive(Debug, Clone)]
pub(super) struct LeafLoadFailureHold {
    pub(super) task_id: String,
    pub(super) leaf_run_id: String,
    pub(super) error: String,
    pub(super) finished_at: DateTime<Utc>,
}

/// This drain's leaves that ended `failed` before any activity step started.
///
/// A load failure (a bad job or activity definition, an invalid catalog
/// entry, a retired declaration, a script manifest that does not resolve)
/// is finalized with a single diagnostic step whose target is the job
/// itself, or with no step at all. A leaf that then fails after real work
/// keeps that same job record: activity rows are persisted only on success.
/// A started activity is an audit `step_started` event. When that event is
/// present the leaf is not held, so ordinary handling re-admits its task.
pub(super) fn leaf_load_failure_holds(
    runtime: &OrbitRuntime,
    action: &str,
    drain_run_id: Option<&str>,
) -> Result<BTreeMap<String, LeafLoadFailureHold>, PassError> {
    let Some(drain_run_id) = drain_run_id else {
        return Ok(BTreeMap::new());
    };
    let Some(state) = runtime
        .read_run_state(drain_run_id)
        .map_err(|error| PassError::store(action, "read drain state for leaf holds", error))?
    else {
        return Ok(BTreeMap::new());
    };
    let jobs = runtime.stores().jobs();
    let mut holds: BTreeMap<String, LeafLoadFailureHold> = BTreeMap::new();
    for dispatch in &state.child_dispatches {
        if dispatch.job_name != LEAF_JOB_NAME {
            continue;
        }
        let Some(run) = jobs
            .get_job_run(&dispatch.child_run_id)
            .map_err(|error| PassError::store(action, "read drain leaf for load hold", error))?
        else {
            continue;
        };
        if run.state != JobRunState::Failed || !failed_at_load(runtime, action, &run)? {
            continue;
        }
        let error = run
            .steps
            .first()
            .and_then(|step| step.error_message.clone())
            .or_else(|| dispatch.error.clone())
            .unwrap_or_else(|| format!("leaf {} failed at job load", run.run_id));
        let finished_at = run.finished_at.unwrap_or_else(|| run.recency_at());
        for task_id in leaf_task_ids(&run) {
            let hold = LeafLoadFailureHold {
                task_id: task_id.clone(),
                leaf_run_id: run.run_id.to_string(),
                error: truncate_error(&error),
                finished_at,
            };
            match holds.get(&task_id) {
                Some(existing) if existing.finished_at >= finished_at => {}
                _ => {
                    holds.insert(task_id, hold);
                }
            }
        }
    }
    Ok(holds)
}

/// Whether `run` failed before any activity step started.
///
/// The job record is the same shape for a load failure and for a later
/// failure: one job-targeted diagnostic, or none. A started activity leaves
/// an audit `step_started` event with a step id, which is what the run
/// projection reconstructs as an activity step. The oldest matching envelope
/// is enough; the pass does not read the rest of a long trail.
fn failed_at_load(
    runtime: &OrbitRuntime,
    action: &str,
    run: &orbit_types::workflow::JobRun,
) -> Result<bool, PassError> {
    if !job_record_shows_only_load_diagnostic(run) {
        return Ok(false);
    }
    Ok(!audit_records_started_step(runtime, action, &run.run_id)?)
}

/// The job-record shape shared by a load failure and a post-step failure.
fn job_record_shows_only_load_diagnostic(run: &orbit_types::workflow::JobRun) -> bool {
    match run.steps.as_slice() {
        [] => true,
        [step] => step.target_type == JobTargetType::Job && step.target_id == run.job_id,
        _ => false,
    }
}

/// Whether the run's audit trail records an activity that started.
fn audit_records_started_step(
    runtime: &OrbitRuntime,
    action: &str,
    run_id: &str,
) -> Result<bool, PassError> {
    let rows = runtime
        .list_v2_audit_events(V2AuditEventFilter {
            run_id: Some(run_id.to_string()),
            source: Some("v2_envelope".to_string()),
            event_type: Some(STARTED_STEP_EVENT_TYPE.to_string()),
            body_kinds: vec![STARTED_STEP_BODY_KIND.to_string()],
            limit: Some(1),
            oldest_first: true,
            ..Default::default()
        })
        .map_err(|error| PassError::store(action, "read leaf audit for load hold", error))?;
    Ok(rows
        .iter()
        .any(|row| payload_names_started_step(&row.payload_json)))
}

/// A `step_started` envelope the run projection would turn into a step.
fn payload_names_started_step(payload: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return false;
    };
    value
        .get("step_id")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|step_id| !step_id.is_empty())
}

fn leaf_task_ids(run: &orbit_types::workflow::JobRun) -> Vec<String> {
    run.input
        .as_ref()
        .and_then(|input| input.get("task_ids"))
        .and_then(serde_json::Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn truncate_error(error: &str) -> String {
    const MAX_BYTES: usize = 512;
    let trimmed = error.trim();
    if trimmed.len() <= MAX_BYTES {
        return trimmed.to_string();
    }
    let mut cut = MAX_BYTES;
    while cut > 0 && !trimmed.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}...", &trimmed[..cut])
}
