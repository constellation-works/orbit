//! Tasks this drain must not re-admit after its own leaf failed at job load.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_types::workflow::{JobRunState, JobTargetType};

use crate::OrbitRuntime;
use crate::application::admission::pass_error::PassError;

use super::super::drains::LEAF_JOB_NAME;

/// One task held for the rest of this drain after its leaf failed at load.
#[derive(Debug, Clone)]
pub(super) struct LeafLoadFailureHold {
    pub(super) task_id: String,
    pub(super) leaf_run_id: String,
    pub(super) error: String,
    pub(super) finished_at: DateTime<Utc>,
}

/// This drain's leaves that ended `failed` before any real step started.
///
/// A load failure (a bad job or activity definition, an invalid catalog
/// entry, a retired declaration, a script manifest that does not resolve)
/// is finalized with a single diagnostic step whose target is the job
/// itself, or with no step at all. A leaf that ran any real step — even
/// one that then failed transiently — carries a non-job step and is not
/// held here, so ordinary handling re-admits its task.
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
        if run.state != JobRunState::Failed || !failed_at_load(&run) {
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

fn failed_at_load(run: &orbit_types::workflow::JobRun) -> bool {
    match run.steps.as_slice() {
        [] => true,
        [step] => step.target_type == JobTargetType::Job && step.target_id == run.job_id,
        _ => false,
    }
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
