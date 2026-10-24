use super::*;

pub(super) fn bounded_id<'a>(value: &'a str, field: &str) -> Result<&'a str, OrbitError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_ID_LEN {
        return Err(OrbitError::InvalidInput(format!(
            "`{field}` must be 1 to {MAX_ID_LEN} characters"
        )));
    }
    Ok(value)
}

/// The tasks the run was submitted with. Only the run's own recorded
/// `task_ids` array counts; a singular `task_id` cannot widen it.
pub(super) fn run_task_ids(run: &JobRun) -> Vec<&str> {
    run.input
        .as_ref()
        .map(recorded_task_ids)
        .unwrap_or_default()
}

/// A step's checkpoint after the consistency check.
pub(super) enum Checkpoint<'a> {
    Recorded(&'a Value),
    Absent,
    Inconsistent,
}

pub(super) fn checkpoint<'a>(state: &'a PipelineState, step: &DeliveryStep) -> Checkpoint<'a> {
    if state.step_states.get(&step.index) != Some(&JobRunState::Success) {
        return Checkpoint::Absent;
    }
    let Some(output) = state.step_output(step.index) else {
        return Checkpoint::Absent;
    };
    // The pipeline entry under the step's id is written by the same host
    // checkpoint. A mismatch means the index no longer names this step (the
    // definition changed since the run) or the record was altered.
    match state.pipeline.get(&step.id) {
        Some(entry) if entry != output => Checkpoint::Inconsistent,
        _ => Checkpoint::Recorded(output),
    }
}

pub(super) fn provenance(step: &DeliveryStep) -> DeliveryEvidenceProvenance {
    DeliveryEvidenceProvenance {
        source: RUN_DELIVERY_EVIDENCE_SOURCE.to_string(),
        source_run_id: None,
        step_id: step.id.clone(),
        step_index: step.index,
        activity: step.activity.to_string(),
    }
}

/// The inherited checkpoint must retain both its logical index and its
/// named pipeline entry, including the exact pointer when terminal compaction
/// moved the indexed bytes into the pipeline.
pub(super) fn inherited_checkpoint_matches(
    state: &PipelineState,
    step: &DeliveryStep,
    output: &Value,
) -> bool {
    let pointer = format!("/{}", step.id.replace('~', "~0").replace('/', "~1"));
    matches!(checkpoint(state, step), Checkpoint::Recorded(value) if value == output)
        && state.pipeline.get(&step.id) == Some(output)
        && state
            .step_output_pointers
            .get(&step.index)
            .is_none_or(|value| value == &pointer)
}

pub(super) fn recorded_provenance(run: &JobRun, step: &DeliveryStep) -> DeliveryEvidenceProvenance {
    DeliveryEvidenceProvenance {
        source_run_id: Some(run.run_id.clone()),
        ..provenance(step)
    }
}

pub(super) fn state_belongs_to(state: &PipelineState, run: &JobRun) -> bool {
    state.run_id == run.run_id.as_str() && state.job_id == run.job_id.as_str()
}

/// Execution indices are tail-local after resume; identity and success are
/// what bind the producing record to the logical definition/checkpoint.
pub(super) fn executed_step(run: &JobRun, step: &DeliveryStep) -> bool {
    run.steps
        .iter()
        .rev()
        .find(|record| record.target_id == step.id.as_str())
        .is_some_and(|record| {
            record.target_type == JobTargetType::Activity && record.state == JobRunState::Success
        })
}

/// When the run's own step record says this step finished.
pub(super) fn step_finished_at(run: &JobRun, step: &DeliveryStep) -> Option<DateTime<Utc>> {
    run.steps
        .iter()
        .rev()
        .find(|record| record.target_id == step.id.as_str())
        .and_then(|record| record.finished_at)
}

pub(super) fn commit_gap(
    reason: DeliveryEvidenceGap,
    step: Option<&DeliveryStep>,
) -> CommitObservation {
    CommitObservation {
        status: CommitObservationStatus::Unavailable,
        base_sha: None,
        head_sha: None,
        observed_at: None,
        provenance: step.map(provenance),
        reason: Some(reason),
    }
}

pub(super) fn commit_without_checkpoint(run: &JobRun, step: &DeliveryStep) -> CommitObservation {
    CommitObservation {
        status: if run.state.is_terminal() {
            CommitObservationStatus::NotReached
        } else {
            CommitObservationStatus::Pending
        },
        base_sha: None,
        head_sha: None,
        observed_at: None,
        provenance: Some(provenance(step)),
        reason: None,
    }
}
