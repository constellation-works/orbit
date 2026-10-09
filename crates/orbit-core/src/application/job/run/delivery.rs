//! [ORB-13744] The bounded delivery observation a non-operator consumer may
//! read for one task-delivery run.
//!
//! `orbit.workflow.run.show` is the operator's whole-run view; this is the
//! narrow public one. Its evidence is the durable checkpoint of a host-run
//! deterministic step, found by the step id the job definition gives it and
//! cross-checked against the run's pipeline entry under that id. Agent step
//! outputs, response envelopes, prompts and step inputs are never read, so an
//! agent that writes a commit claim anywhere it can reach changes nothing here.
//!
//! Ownership fails closed before any evidence is read: the run must exist in
//! this workspace's run store, the task in its task store, the run's own
//! submitted `task_ids` must name the task, and the job must hold task
//! delivery. Evidence that is missing or does not match is reported as a typed
//! gap, never filled from a weaker source. A claimed leaf's task lives in its
//! owner's store instead, so its executor answers for the leaf on the durable
//! admission binding the leaf to that task's claim.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::workflow::{
    ActivityV2Spec, CommitObservation, CommitObservationStatus, DeliveryEvidenceGap,
    DeliveryEvidenceProvenance, JobRun, JobRunState, JobV2, JobV2Step, JobV2StepBody,
    LandingMethod, LandingObservation, LandingObservationStatus, PipelineState,
    RUN_DELIVERY_EVIDENCE_SOURCE, RUN_DELIVERY_SCHEMA_VERSION, RunDeliveryObservation,
    RunDeliveryStatus, TargetStep,
};
use serde_json::Value;

use crate::OrbitRuntime;

const COMMIT_ACTIVITY: &str = "git_commit";
const PR_LANDING_ACTIVITY: &str = "pr_complete";
const LOCAL_LANDING_ACTIVITY: &str = "git_merge";

/// Longest run or task id this read accepts.
const MAX_ID_LEN: usize = 128;
/// Longest PR number accepted from a checkpoint.
const MAX_PR_NUMBER_DIGITS: usize = 12;

/// One top-level host step the observation reads, located in the job
/// definition rather than guessed from a step index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeliveryStep {
    pub(crate) id: String,
    pub(crate) index: u32,
    pub(crate) activity: &'static str,
}

/// The delivery-relevant steps of one job definition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DeliveryJobShape {
    pub(crate) commit: Option<DeliveryStep>,
    pub(crate) landings: Vec<DeliveryStep>,
}

impl DeliveryJobShape {
    /// Only resolved catalog targets whose spec is the deterministic host
    /// action count: an agent activity that happens to be named like one is
    /// not host evidence.
    pub(crate) fn from_job(job: &JobV2) -> Self {
        let mut shape = Self::default();
        for (index, step) in job.steps.iter().enumerate() {
            let Ok(index) = u32::try_from(index) else {
                break;
            };
            let Some(activity) = host_activity(step) else {
                continue;
            };
            let step = DeliveryStep {
                id: step.id.clone(),
                index,
                activity,
            };
            if activity == COMMIT_ACTIVITY {
                if shape.commit.is_none() {
                    shape.commit = Some(step);
                }
            } else {
                shape.landings.push(step);
            }
        }
        shape
    }
}

fn host_activity(step: &JobV2Step) -> Option<&'static str> {
    let JobV2StepBody::Target(TargetStep {
        spec: ActivityV2Spec::Deterministic(spec),
        activity_name: Some(name),
        ..
    }) = &step.body
    else {
        return None;
    };
    [COMMIT_ACTIVITY, PR_LANDING_ACTIVITY, LOCAL_LANDING_ACTIVITY]
        .into_iter()
        .find(|activity| name == activity && spec.action == *activity)
}

impl OrbitRuntime {
    /// Observe what one task-delivery run committed and landed for one task
    /// of this workspace. Read-only: the run is read as stored, without the
    /// stale-run reconciliation an operator read performs.
    pub fn observe_run_delivery(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<RunDeliveryObservation, OrbitError> {
        let run_id = bounded_id(run_id, "run_id")?;
        let task_id = bounded_id(task_id, "task_id")?;
        let run = self.show_job_run_observed(run_id)?;
        let task = self.get_task(task_id)?;
        if !run_task_ids(&run).contains(&task.id.as_str()) {
            return Err(OrbitError::InvalidInput(format!(
                "run '{run_id}' did not deliver task '{task_id}' in this workspace"
            )));
        }
        self.project_stored_delivery(&run, &task.id)
    }

    /// [ORB-14661] Observe what a claimed leaf run committed and landed for
    /// its claimed task, from this executor's own record of the leaf. The
    /// claimed task lives in its owner's store, never this one, so ownership
    /// is the durable admission binding the leaf to the claim on that task,
    /// already authorized by the caller, in place of the task-store lookup
    /// [`Self::observe_run_delivery`] makes. The owner holds no record of the
    /// leaf, so only this side can answer for it.
    pub(crate) fn observe_claimed_leaf_delivery(
        &self,
        leaf: &crate::application::job::claimed::ClaimedLeaf,
    ) -> Result<RunDeliveryObservation, OrbitError> {
        let run_id = leaf.binding.bound_run_id.as_str();
        let task_id = leaf.claim.task_id.as_str();
        let run = self.show_job_run_observed(run_id)?;
        if !run_task_ids(&run).contains(&task_id) {
            return Err(OrbitError::InvalidInput(format!(
                "run '{run_id}' did not deliver its claimed task"
            )));
        }
        self.project_stored_delivery(&run, task_id)
    }

    /// The observation of `run` for `task_id`, whose ownership the caller
    /// has already established.
    fn project_stored_delivery(
        &self,
        run: &JobRun,
        task_id: &str,
    ) -> Result<RunDeliveryObservation, OrbitError> {
        let run_id = run.run_id.as_str();
        let shape = match self.load_v2_job_asset_by_name(&run.job_id) {
            Ok((_, mut job)) => {
                if !job.holds_task_delivery() {
                    return Err(OrbitError::InvalidInput(format!(
                        "run '{run_id}' is not a task delivery run"
                    )));
                }
                // Resolve `activity:` refs exactly as execution does, so the
                // spec judged below is the one the run's steps ran.
                self.v2_activity_catalog()
                    .ok()
                    .and_then(|catalog| {
                        orbit_engine::resolve_job_catalog_refs_for_execution(&mut job, &catalog)
                            .ok()
                    })
                    .map(|()| DeliveryJobShape::from_job(&job))
            }
            Err(_) => None,
        };
        let state = self.read_run_state(run_id)?;
        let repository =
            crate::application::automation::source::Source::new(&self.paths().repo_root)
                .repository()
                .ok();
        Ok(project_run_delivery(
            self.workspace_id()?,
            repository,
            run,
            state.as_ref(),
            shape.as_ref(),
            task_id,
        ))
    }
}

impl OrbitRuntime {
    /// Observe what a delivery run committed and landed for one task: the
    /// named run, or, when none is named, the newest task-delivery run this
    /// workspace recorded with the task among its submitted `task_ids`.
    ///
    /// The run is chosen from the same evidence the observation checks — the
    /// run's own submitted input and a job that holds task delivery — so the
    /// default never picks a run the explicit form would refuse.
    pub fn observe_task_delivery(
        &self,
        task_id: &str,
        run_id: Option<&str>,
    ) -> Result<RunDeliveryObservation, OrbitError> {
        if let Some(run_id) = run_id {
            return self.observe_run_delivery(run_id, task_id);
        }
        let task_id = bounded_id(task_id, "task_id")?;
        let task = self.get_task(task_id)?;
        let run_id = self.latest_delivery_run_id(&task.id)?;
        self.observe_run_delivery(&run_id, &task.id)
    }

    fn latest_delivery_run_id(&self, task_id: &str) -> Result<String, OrbitError> {
        // Filter submitted task bindings before hydrating run inputs, newest
        // first. No global cap: newer non-delivery jobs must not hide the
        // latest delivery run. Steps are not needed to choose the run.
        let runs = self.list_job_runs_filtered_backend(&JobRunQuery {
            task_id: Some(task_id.to_string()),
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        let mut delivers = HashMap::<String, bool>::new();
        for run in runs {
            if !run_task_ids(&run).contains(&task_id) {
                continue;
            }
            let holds_delivery = *delivers.entry(run.job_id.clone()).or_insert_with(|| {
                self.load_v2_job_asset_by_name(&run.job_id)
                    .is_ok_and(|(_, job)| job.holds_task_delivery())
            });
            if holds_delivery {
                return Ok(run.run_id);
            }
        }
        Err(OrbitError::InvalidInput(format!(
            "no task delivery run in this workspace was submitted with task '{task_id}'"
        )))
    }
}

fn bounded_id<'a>(value: &'a str, field: &str) -> Result<&'a str, OrbitError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_ID_LEN {
        return Err(OrbitError::InvalidInput(format!(
            "`{field}` must be 1 to {MAX_ID_LEN} characters"
        )));
    }
    Ok(value)
}

/// The tasks the run was submitted with. Only the run's own recorded input
/// counts; a caller cannot widen it.
fn run_task_ids(run: &JobRun) -> Vec<&str> {
    run.input
        .as_ref()
        .and_then(|input| input.get("task_ids"))
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// Project durable records into the public observation. Pure, so every
/// partial and corrupt shape can be exercised without running a pipeline.
pub(crate) fn project_run_delivery(
    workspace_id: String,
    repository: Option<String>,
    run: &JobRun,
    state: Option<&PipelineState>,
    shape: Option<&DeliveryJobShape>,
    task_id: &str,
) -> RunDeliveryObservation {
    let commit = observe_commit(run, state, shape, task_id);
    let landing = observe_landing(run, state, shape, &commit);
    let delivery_status = summarize(run.state, &commit, &landing);
    RunDeliveryObservation {
        schema_version: RUN_DELIVERY_SCHEMA_VERSION,
        workspace_id,
        repository,
        task_id: task_id.to_string(),
        run_id: run.run_id.clone(),
        job_id: run.job_id.clone(),
        run_state: run.state,
        run_finished_at: run.finished_at,
        delivery_status,
        commit,
        landing,
    }
}

/// A step's checkpoint after the consistency check.
enum Checkpoint<'a> {
    Recorded(&'a Value),
    Absent,
    Inconsistent,
}

fn checkpoint<'a>(state: &'a PipelineState, step: &DeliveryStep) -> Checkpoint<'a> {
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

fn provenance(step: &DeliveryStep) -> DeliveryEvidenceProvenance {
    DeliveryEvidenceProvenance {
        source: RUN_DELIVERY_EVIDENCE_SOURCE.to_string(),
        step_id: step.id.clone(),
        step_index: step.index,
        activity: step.activity.to_string(),
    }
}

/// When the run's own step record says this step finished.
fn step_finished_at(run: &JobRun, step: &DeliveryStep) -> Option<DateTime<Utc>> {
    run.steps
        .iter()
        .rev()
        .find(|record| record.target_id == step.id.as_str())
        .and_then(|record| record.finished_at)
}

fn commit_gap(reason: DeliveryEvidenceGap, step: Option<&DeliveryStep>) -> CommitObservation {
    CommitObservation {
        status: CommitObservationStatus::Unavailable,
        base_sha: None,
        head_sha: None,
        observed_at: None,
        provenance: step.map(provenance),
        reason: Some(reason),
    }
}

fn commit_without_checkpoint(run: &JobRun, step: &DeliveryStep) -> CommitObservation {
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

fn observe_commit(
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
            provenance: Some(provenance(step)),
            reason: None,
        },
        Err(gap) => commit_gap(gap, Some(step)),
    }
}

type ParsedCommit = (CommitObservationStatus, Option<String>, Option<String>);

/// Read the `git_commit` output contract. Unknown decisions, a missing head
/// for a performed commit, or a malformed SHA all refuse rather than guess.
fn parse_commit_output(
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
    if (status == CommitObservationStatus::Committed) != head_sha.is_some() {
        return Err(DeliveryEvidenceGap::OutputMalformed);
    }
    Ok((status, base_sha, head_sha))
}

fn optional_sha(value: Option<&Value>) -> Result<Option<String>, DeliveryEvidenceGap> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(sha)) if is_full_sha(sha) => Ok(Some(sha.clone())),
        Some(_) => Err(DeliveryEvidenceGap::OutputMalformed),
    }
}

/// A full SHA-1 or SHA-256 object id in lowercase hex.
fn is_full_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn landing(status: LandingObservationStatus) -> LandingObservation {
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

fn landing_gap(reason: DeliveryEvidenceGap, step: Option<&DeliveryStep>) -> LandingObservation {
    LandingObservation {
        provenance: step.map(provenance),
        reason: Some(reason),
        ..landing(LandingObservationStatus::Unavailable)
    }
}

fn observe_landing(
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
        match parse_landing_output(step, output) {
            Ok(Some(merged)) => {
                return LandingObservation {
                    observed_at: step_finished_at(run, step),
                    provenance: Some(provenance(step)),
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

fn not_yet(run: &JobRun) -> LandingObservationStatus {
    if run.state.is_terminal() {
        LandingObservationStatus::NotReached
    } else {
        LandingObservationStatus::Pending
    }
}

/// `Ok(None)` for a landing step whose recorded route merged nothing.
fn parse_landing_output(
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

fn summarize(
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
