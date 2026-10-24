use super::*;

pub(super) fn observe_commit(
    run: &JobRun,
    state: Option<&PipelineState>,
    shape: Option<&DeliveryJobShape>,
    task_id: &str,
) -> CommitObservation {
    let Some(shape) = shape else {
        return commit_gap(DeliveryEvidenceGap::JobDefinitionUnavailable, None);
    };
    let Some(step) = shape.commit.as_ref() else {
        return commit_gap(DeliveryEvidenceGap::NoCommitStep, None);
    };
    let Some(state) = state else {
        return if run.state.is_terminal() {
            commit_gap(DeliveryEvidenceGap::StateMissing, Some(step))
        } else {
            commit_without_checkpoint(run, step)
        };
    };
    if !state_belongs_to(state, run) {
        return commit_gap(DeliveryEvidenceGap::OwnershipMismatch, Some(step));
    }
    let output = match checkpoint(state, step) {
        Checkpoint::Recorded(output) => output,
        Checkpoint::Absent => return commit_without_checkpoint(run, step),
        Checkpoint::Inconsistent => {
            return commit_gap(DeliveryEvidenceGap::CheckpointInconsistent, Some(step));
        }
    };
    match parse_commit_output(output, &run.run_id, task_id) {
        Ok((status, base_sha, head_sha)) => CommitObservation {
            status,
            base_sha,
            head_sha,
            observed_at: step_finished_at(run, step),
            provenance: Some(recorded_provenance(run, step)),
            reason: None,
        },
        Err(gap) => commit_gap(gap, Some(step)),
    }
}

pub(super) type ParsedCommit = (CommitObservationStatus, Option<String>, Option<String>);

/// Read the `git_commit` output contract. Unknown decisions, a missing head
/// for a performed commit, or a malformed SHA all refuse rather than guess.
pub(super) fn parse_commit_output(
    output: &Value,
    run_id: &str,
    task_id: &str,
) -> Result<ParsedCommit, DeliveryEvidenceGap> {
    let object = output
        .as_object()
        .ok_or(DeliveryEvidenceGap::OutputMalformed)?;
    if object.get("phase").and_then(Value::as_str) != Some("commit") {
        return Err(DeliveryEvidenceGap::OutputMalformed);
    }
    if object.get("task_id").and_then(Value::as_str) != Some(task_id) {
        return Err(DeliveryEvidenceGap::OwnershipMismatch);
    }
    match object.get("job_run_id") {
        None => {}
        Some(Value::String(recorded)) if recorded == run_id => {}
        Some(_) => return Err(DeliveryEvidenceGap::OwnershipMismatch),
    }
    let status = match object.get("decision").and_then(Value::as_str) {
        Some("performed") => CommitObservationStatus::Committed,
        Some("already_committed") => CommitObservationStatus::AlreadyCommitted,
        Some("verified_no_diff") => CommitObservationStatus::VerifiedNoDiff,
        Some("verified_already_landed") => CommitObservationStatus::VerifiedAlreadyLanded,
        Some("skipped_no_diff_expected") => CommitObservationStatus::SkippedNoDiffExpected,
        _ => return Err(DeliveryEvidenceGap::OutputMalformed),
    };
    let committed = object.get("committed").and_then(Value::as_bool);
    if committed != Some(status == CommitObservationStatus::Committed) {
        return Err(DeliveryEvidenceGap::OutputMalformed);
    }
    let base_sha = optional_sha(object.get("base_sha"))?;
    let head_sha = optional_sha(object.get("commit_sha"))?;
    let replay = object.get("final_recovery_replay").and_then(Value::as_bool) == Some(true);
    if replay
        && (object.get("job_run_id").and_then(Value::as_str) != Some(run_id)
            || object
                .get("candidate_branch")
                .and_then(Value::as_str)
                .is_none_or(|branch| branch.trim().is_empty())
            || base_sha.is_none()
            || !matches!(
                status,
                CommitObservationStatus::Committed | CommitObservationStatus::AlreadyCommitted
            ))
    {
        return Err(DeliveryEvidenceGap::OutputMalformed);
    }
    // A host replay receipt pins the existing candidate without claiming to
    // have created it. Legacy already-committed outputs still carry no SHA;
    // a bare SHA without the replay binding remains malformed.
    let pins_head = status == CommitObservationStatus::Committed
        || (replay && status == CommitObservationStatus::AlreadyCommitted);
    if pins_head != head_sha.is_some() {
        return Err(DeliveryEvidenceGap::OutputMalformed);
    }
    Ok((status, base_sha, head_sha))
}

pub(super) fn optional_sha(value: Option<&Value>) -> Result<Option<String>, DeliveryEvidenceGap> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(sha)) if is_full_sha(sha) => Ok(Some(sha.clone())),
        Some(_) => Err(DeliveryEvidenceGap::OutputMalformed),
    }
}

/// A full SHA-1 or SHA-256 object id in lowercase hex.
pub(super) fn is_full_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn landing(status: LandingObservationStatus) -> LandingObservation {
    LandingObservation {
        status,
        method: None,
        landed_commit: None,
        pr_number: None,
        observed_at: None,
        provenance: None,
        reason: None,
    }
}

pub(super) fn landing_gap(
    reason: DeliveryEvidenceGap,
    step: Option<&DeliveryStep>,
) -> LandingObservation {
    LandingObservation {
        provenance: step.map(provenance),
        reason: Some(reason),
        ..landing(LandingObservationStatus::Unavailable)
    }
}

pub(super) fn observe_landing(
    run: &JobRun,
    state: Option<&PipelineState>,
    shape: Option<&DeliveryJobShape>,
    commit: &CommitObservation,
) -> LandingObservation {
    let Some(shape) = shape else {
        return landing_gap(DeliveryEvidenceGap::JobDefinitionUnavailable, None);
    };
    if shape.landings.is_empty() {
        return landing(LandingObservationStatus::NotApplicable);
    }
    // No landing is verified for a run whose commit evidence is unusable, and
    // a landing record without the commit it lands is not read at all.
    match commit.status {
        CommitObservationStatus::Unavailable => {
            return landing(LandingObservationStatus::Unavailable);
        }
        CommitObservationStatus::Pending | CommitObservationStatus::NotReached => {
            return landing(not_yet(run));
        }
        _ => {}
    }
    let Some(state) = state else {
        return landing(not_yet(run));
    };
    let mut skipped = 0;
    for step in &shape.landings {
        let output = match checkpoint(state, step) {
            Checkpoint::Recorded(output) => output,
            Checkpoint::Absent => continue,
            Checkpoint::Inconsistent => {
                return landing_gap(DeliveryEvidenceGap::CheckpointInconsistent, Some(step));
            }
        };
        // A `when:`-skipped step checkpoints a null output.
        if output.is_null() {
            skipped += 1;
            continue;
        }
        if !state_belongs_to(state, run) {
            return landing_gap(DeliveryEvidenceGap::OwnershipMismatch, Some(step));
        }
        // A retry's current landing needs its own successful host step record.
        // Copied landing bytes cannot establish a new merge on this run.
        if run.retry_source_run_id.is_some() {
            if !executed_step(run, step) {
                return landing_gap(DeliveryEvidenceGap::OwnershipMismatch, Some(step));
            }
            if !inherited_checkpoint_matches(state, step, output) {
                return landing_gap(DeliveryEvidenceGap::CheckpointInconsistent, Some(step));
            }
        }
        match parse_landing_output(step, output) {
            Ok(Some(merged)) => {
                return LandingObservation {
                    observed_at: step_finished_at(run, step),
                    provenance: Some(recorded_provenance(run, step)),
                    ..merged
                };
            }
            // The step ran on a route that merges nothing (no-change
            // completion); it is not a landing.
            Ok(None) => skipped += 1,
            Err(gap) => return landing_gap(gap, Some(step)),
        }
    }
    if skipped == shape.landings.len() {
        landing(LandingObservationStatus::NotRequested)
    } else if run.state == JobRunState::Success && skipped > 0 {
        // A successful run checkpoints every top-level step it ran.
        landing(LandingObservationStatus::NotRequested)
    } else {
        landing(not_yet(run))
    }
}

pub(super) fn not_yet(run: &JobRun) -> LandingObservationStatus {
    if run.state.is_terminal() {
        LandingObservationStatus::NotReached
    } else {
        LandingObservationStatus::Pending
    }
}

/// `Ok(None)` for a landing step whose recorded route merged nothing.
pub(super) fn parse_landing_output(
    step: &DeliveryStep,
    output: &Value,
) -> Result<Option<LandingObservation>, DeliveryEvidenceGap> {
    let object = output
        .as_object()
        .ok_or(DeliveryEvidenceGap::OutputMalformed)?;
    if step.activity == LOCAL_LANDING_ACTIVITY {
        // `git_merge` records `{}` when the run carried no task, and the
        // fast-forward target otherwise. The landed SHA is not recorded.
        return match object.get("base").and_then(Value::as_str) {
            Some(_) => Ok(Some(LandingObservation {
                method: Some(LandingMethod::LocalFastForward),
                ..landing(LandingObservationStatus::Merged)
            })),
            None if object.is_empty() => Ok(None),
            None => Err(DeliveryEvidenceGap::OutputMalformed),
        };
    }
    if object.get("phase").and_then(Value::as_str) != Some("complete") {
        return Err(DeliveryEvidenceGap::OutputMalformed);
    }
    let merge = object
        .get("merge")
        .and_then(Value::as_object)
        .ok_or(DeliveryEvidenceGap::OutputMalformed)?;
    match merge.get("merged").and_then(Value::as_bool) {
        Some(true) => {}
        Some(false) => return Ok(None),
        None => return Err(DeliveryEvidenceGap::OutputMalformed),
    }
    let landed_commit = optional_sha(merge.get("landed_commit"))?;
    let pr_number = match merge.get("pr_number") {
        None | Some(Value::Null) => None,
        Some(Value::String(raw))
            if !raw.is_empty()
                && raw.len() <= MAX_PR_NUMBER_DIGITS
                && raw.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            raw.parse::<u64>().ok()
        }
        Some(Value::Number(number)) => number.as_u64(),
        Some(_) => return Err(DeliveryEvidenceGap::OutputMalformed),
    };
    Ok(Some(LandingObservation {
        method: Some(LandingMethod::PullRequest),
        landed_commit,
        pr_number,
        ..landing(LandingObservationStatus::Merged)
    }))
}

pub(super) fn summarize(
    run_state: JobRunState,
    commit: &CommitObservation,
    landing: &LandingObservation,
) -> RunDeliveryStatus {
    match commit.status {
        CommitObservationStatus::Unavailable => return RunDeliveryStatus::Unavailable,
        status if landing.status == LandingObservationStatus::Merged && !status.is_no_change() => {
            return RunDeliveryStatus::Landed;
        }
        _ => {}
    }
    if landing.status == LandingObservationStatus::Unavailable {
        return RunDeliveryStatus::Unavailable;
    }
    match commit.status {
        CommitObservationStatus::Committed | CommitObservationStatus::AlreadyCommitted => {
            RunDeliveryStatus::Committed
        }
        status if status.is_no_change() && run_state == JobRunState::Success => {
            RunDeliveryStatus::NoChange
        }
        _ if !run_state.is_terminal() => RunDeliveryStatus::InProgress,
        _ => RunDeliveryStatus::NotDelivered,
    }
}
