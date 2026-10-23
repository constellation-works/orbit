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
//! gap, never filled from a weaker source. A codebase scan observes tasks it
//! read from this workspace's own task listing, which stands in for the task
//! lookup. A claimed leaf's task lives in its owner's store instead, so its
//! executor answers for the leaf on the durable admission binding the leaf to
//! that task's claim.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_engine::activity_job::V2ActivityCatalog;
use orbit_store::contracts::JobRunQuery;
use orbit_types::workflow::{
    ActivityV2Spec, CommitObservation, CommitObservationStatus, DeliveryEvidenceGap,
    DeliveryEvidenceProvenance, JobRun, JobRunState, JobTargetType, JobV2, JobV2Step,
    JobV2StepBody, LandingMethod, LandingObservation, LandingObservationStatus, PipelineState,
    RUN_DELIVERY_EVIDENCE_SOURCE, RUN_DELIVERY_SCHEMA_VERSION, RunDeliveryObservation,
    RunDeliveryStatus, TargetStep,
};
use serde_json::Value;

use crate::runtime::run_input::recorded_task_ids;

use crate::OrbitRuntime;
use crate::application::job::catalog::V2JobNameCache;

const COMMIT_ACTIVITY: &str = "git_commit";
const PR_LANDING_ACTIVITY: &str = "pr_complete";
const LOCAL_LANDING_ACTIVITY: &str = "git_merge";

/// Longest run or task id this read accepts.
const MAX_ID_LEN: usize = 128;
/// Only parent links are read; no sibling or broad-history search.
const MAX_ANCESTOR_HOPS: usize = 64;
/// Longest PR number accepted from a checkpoint.
const MAX_PR_NUMBER_DIGITS: usize = 12;

/// One top-level host step the observation reads, located in the job
/// definition rather than guessed from a step index.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeliveryStep {
    pub(crate) id: String,
    pub(crate) index: u32,
    pub(crate) activity: &'static str,
    definition: JobV2Step,
}

/// The delivery-relevant steps of one job definition.
#[derive(Debug, Clone, Default, PartialEq)]
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
                definition: step.clone(),
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

/// The catalogs one observation resolves definitions from, each parsed at
/// most once however many runs it reads.
#[derive(Default)]
struct DeliveryDefinitions {
    jobs: V2JobNameCache,
    activities: OnceCell<Option<V2ActivityCatalog>>,
}

/// Run and job ids by submitted task, newest run first.
type RunsByTask = HashMap<String, Vec<(String, String)>>;

/// What a codebase scan reuses across the delivery observations it makes:
/// the repository identity, this workspace's runs indexed by submitted task,
/// and the definitions those runs resolve. Each is read once, on the first
/// observation that needs it.
#[derive(Default)]
pub(crate) struct CodebaseDeliveryScan {
    repository: OnceCell<Option<String>>,
    runs: OnceCell<Result<RunsByTask, String>>,
    definitions: DeliveryDefinitions,
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
        self.project_stored_delivery(
            &run,
            &task.id,
            || self.delivery_repository(),
            &DeliveryDefinitions::default(),
        )
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
        self.project_stored_delivery(
            &run,
            task_id,
            || self.delivery_repository(),
            &DeliveryDefinitions::default(),
        )
    }

    /// The observation of `run` for `task_id`, whose ownership the caller
    /// has already established.
    fn project_stored_delivery(
        &self,
        run: &JobRun,
        task_id: &str,
        repository: impl FnOnce() -> Option<String>,
        definitions: &DeliveryDefinitions,
    ) -> Result<RunDeliveryObservation, OrbitError> {
        let run_id = run.run_id.as_str();
        let shape = self.delivery_shape(run, definitions)?;
        let state = self.read_run_state(run_id)?;
        let repository = repository();
        let executed = shape
            .as_ref()
            .map(|shape| self.delivery_execution_records(run, shape));
        let commit = match &executed {
            Some(Err(gap)) => {
                commit_gap(*gap, shape.as_ref().and_then(|shape| shape.commit.as_ref()))
            }
            _ => self.observe_stored_commit(
                executed
                    .as_ref()
                    .and_then(|run| run.as_ref().ok())
                    .unwrap_or(run),
                state.as_ref(),
                shape.as_ref(),
                task_id,
                definitions,
            ),
        };
        let landing = observe_landing(
            executed
                .as_ref()
                .and_then(|run| run.as_ref().ok())
                .unwrap_or(run),
            state.as_ref(),
            shape.as_ref(),
            &commit,
        );
        let delivery_status = summarize(run.state, &commit, &landing);
        Ok(RunDeliveryObservation {
            schema_version: RUN_DELIVERY_SCHEMA_VERSION,
            workspace_id: self.workspace_id()?,
            repository,
            task_id: task_id.to_string(),
            run_id: run.run_id.clone(),
            job_id: run.job_id.clone(),
            run_state: run.state,
            run_finished_at: run.finished_at,
            delivery_status,
            commit,
            landing,
        })
    }

    /// Resolve catalog refs exactly as execution does, using the pinned
    /// definition where one exists. An unresolved or non-host action never
    /// authenticates a checkpoint.
    fn delivery_shape(
        &self,
        run: &JobRun,
        definitions: &DeliveryDefinitions,
    ) -> Result<Option<DeliveryJobShape>, OrbitError> {
        let Some(mut job) = self.run_definition(&run.run_id, &run.job_id, definitions) else {
            return Ok(None);
        };
        if !job.holds_task_delivery() {
            return Err(OrbitError::InvalidInput(format!(
                "run '{}' is not a task delivery run",
                run.run_id
            )));
        }
        let activities = definitions
            .activities
            .get_or_init(|| self.v2_activity_catalog().ok());
        Ok(activities.as_ref().and_then(|catalog| {
            orbit_engine::resolve_job_catalog_refs_for_execution(&mut job, catalog).ok()?;
            Some(DeliveryJobShape::from_job(&job))
        }))
    }

    /// The definition [`Self::resolve_run_definition`] resolves for a run:
    /// its pinned snapshot, else the catalog job it names. `None` when
    /// neither can be read.
    fn run_definition(
        &self,
        run_id: &str,
        job_id: &str,
        definitions: &DeliveryDefinitions,
    ) -> Option<JobV2> {
        match self.read_run_definition_snapshot(run_id) {
            Ok(Some((job, _))) => Some(job),
            Ok(None) => self.v2_job_by_name_cached(&definitions.jobs, job_id),
            Err(_) => None,
        }
    }

    fn observe_stored_commit(
        &self,
        run: &JobRun,
        state: Option<&PipelineState>,
        shape: Option<&DeliveryJobShape>,
        task_id: &str,
        definitions: &DeliveryDefinitions,
    ) -> CommitObservation {
        let ordinary = observe_commit(run, state, shape, task_id);
        let (Some(state), Some(step)) = (state, shape.and_then(|shape| shape.commit.as_ref()))
        else {
            return ordinary;
        };
        if !state_belongs_to(state, run) {
            return commit_gap(DeliveryEvidenceGap::OwnershipMismatch, Some(step));
        }
        let Checkpoint::Recorded(output) = checkpoint(state, step) else {
            return ordinary;
        };
        let owner = output.get("job_run_id").and_then(Value::as_str);
        // Older checkpoints omit the owner. When a retry has no executed
        // commit record, authenticate its producer rather than attributing the
        // copied bytes (and a tail-local record's timestamp) to the retry.
        let inherited = owner.is_some_and(|id| id != run.run_id.as_str())
            || (run.retry_source_run_id.is_some() && !executed_step(run, step));
        if !inherited {
            return ordinary;
        }
        match self.inherited_commit(run, state, step, output, task_id, definitions) {
            Ok(commit) => commit,
            Err(gap) => commit_gap(gap, Some(step)),
        }
    }

    /// Authenticate every parent link all the way to the root, even after
    /// finding the producer, so a cycle or truncated lineage cannot hide
    /// behind a matching checkpoint. Only the copied prefix through the
    /// producer must contain identical checkpoint bytes.
    fn inherited_commit(
        &self,
        run: &JobRun,
        current_state: &PipelineState,
        step: &DeliveryStep,
        output: &Value,
        task_id: &str,
        definitions: &DeliveryDefinitions,
    ) -> Result<CommitObservation, DeliveryEvidenceGap> {
        if !inherited_checkpoint_matches(current_state, step, output) {
            return Err(DeliveryEvidenceGap::CheckpointInconsistent);
        }
        let mut seen = HashSet::from([run.run_id.clone()]);
        let mut cursor = run.retry_source_run_id.clone();
        let mut producer = None;
        let mut hops = 0;
        while let Some(parent_id) = cursor {
            if hops >= MAX_ANCESTOR_HOPS
                || bounded_id(&parent_id, "run_id").is_err()
                || !seen.insert(parent_id.clone())
            {
                return Err(DeliveryEvidenceGap::OwnershipMismatch);
            }
            hops += 1;
            // This query is scoped by the answering runtime's workspace.
            let parent = self
                .show_job_run_observed(&parent_id)
                .map_err(|_| DeliveryEvidenceGap::OwnershipMismatch)?;
            if parent.job_id != run.job_id || !run_task_ids(&parent).contains(&task_id) {
                return Err(DeliveryEvidenceGap::OwnershipMismatch);
            }
            let shape = self
                .delivery_shape(&parent, definitions)
                .ok()
                .flatten()
                .ok_or(DeliveryEvidenceGap::JobDefinitionUnavailable)?;
            if shape.commit.as_ref() != Some(step) {
                return Err(DeliveryEvidenceGap::CheckpointInconsistent);
            }
            let parent = self.delivery_execution_records(&parent, &shape)?;
            let state = self
                .read_run_state(&parent_id)
                .ok()
                .flatten()
                .ok_or(DeliveryEvidenceGap::StateMissing)?;
            if !state_belongs_to(&state, &parent) {
                return Err(DeliveryEvidenceGap::OwnershipMismatch);
            }
            if producer.is_none() {
                if !inherited_checkpoint_matches(&state, step, output) {
                    return Err(DeliveryEvidenceGap::CheckpointInconsistent);
                }
                if executed_step(&parent, step) {
                    // The producer's recognized host contract checks both task
                    // and run ownership. An ancestor merely named in input is
                    // insufficient, even if the retry claims success.
                    parse_commit_output(output, &parent.run_id, task_id)?;
                    producer = Some(observe_commit(&parent, Some(&state), Some(&shape), task_id));
                }
            }
            cursor = parent.retry_source_run_id;
        }
        producer.ok_or(DeliveryEvidenceGap::OwnershipMismatch)
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
        // Filter submitted task bindings before hydrating run inputs, newest
        // first. No global cap: newer non-delivery jobs must not hide the
        // latest delivery run. Steps are not needed to choose the run.
        let runs = self.list_job_runs_filtered_backend(&JobRunQuery {
            task_id: Some(task.id.clone()),
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        let run_id = self.latest_delivery_run_id(
            &task.id,
            runs.iter()
                .filter(|run| run_task_ids(run).contains(&task.id.as_str()))
                .map(|run| (run.run_id.as_str(), run.job_id.as_str())),
            &DeliveryDefinitions::default(),
        )?;
        self.observe_run_delivery(&run_id, &task.id)
    }

    /// [`Self::observe_task_delivery`] for one task of a codebase scan, which
    /// read `task_id` from this workspace's own task listing: that listing
    /// establishes the task's ownership, so the task is not read again. The
    /// run is still read, and must name the task, before any evidence is.
    /// Everything `scan` holds is resolved once across the scan, including
    /// a failed resolution.
    pub(crate) fn observe_codebase_task_delivery(
        &self,
        task_id: &str,
        scan: &CodebaseDeliveryScan,
    ) -> Result<RunDeliveryObservation, OrbitError> {
        tracing::debug!(task_id, codebase_delivery_observation = true);
        let task_id = bounded_id(task_id, "task_id")?;
        let runs = scan
            .runs
            .get_or_init(|| {
                self.delivery_runs_by_task()
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|error| OrbitError::Store(error.clone()))?;
        let run_id = self.latest_delivery_run_id(
            task_id,
            runs.get(task_id)
                .into_iter()
                .flatten()
                .map(|(run_id, job_id)| (run_id.as_str(), job_id.as_str())),
            &scan.definitions,
        )?;
        let run = self.show_job_run_observed(&run_id)?;
        if !run_task_ids(&run).contains(&task_id) {
            return Err(OrbitError::InvalidInput(format!(
                "run '{run_id}' did not deliver task '{task_id}' in this workspace"
            )));
        }
        self.project_stored_delivery(
            &run,
            task_id,
            || {
                scan.repository
                    .get_or_init(|| self.delivery_repository())
                    .clone()
            },
            &scan.definitions,
        )
    }

    /// Every run of this workspace by the tasks it was submitted with, newest
    /// first, from one listing without steps.
    fn delivery_runs_by_task(&self) -> Result<RunsByTask, OrbitError> {
        let mut by_task = RunsByTask::new();
        let runs = self.list_job_runs_filtered_backend(&JobRunQuery {
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        for run in &runs {
            for task_id in run_task_ids(run).into_iter().collect::<HashSet<_>>() {
                by_task
                    .entry(task_id.to_string())
                    .or_default()
                    .push((run.run_id.clone(), run.job_id.clone()));
            }
        }
        Ok(by_task)
    }

    fn delivery_repository(&self) -> Option<String> {
        crate::application::automation::source::Source::new(&self.paths().repo_root)
            .repository()
            .ok()
    }

    /// The first of `runs`, the task's runs newest first as `(run_id,
    /// job_id)`, whose definition holds task delivery. A direct-path run
    /// judges its own pinned snapshot; catalog-backed runs of one job share
    /// the catalog's verdict.
    fn latest_delivery_run_id<'a>(
        &self,
        task_id: &str,
        runs: impl Iterator<Item = (&'a str, &'a str)>,
        definitions: &DeliveryDefinitions,
    ) -> Result<String, OrbitError> {
        for (run_id, job_id) in runs {
            if self
                .run_definition(run_id, job_id, definitions)
                .is_some_and(|job| job.holds_task_delivery())
            {
                return Ok(run_id.to_string());
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

/// The tasks the run was submitted with. Only the run's own recorded
/// `task_ids` array counts; a singular `task_id` cannot widen it.
fn run_task_ids(run: &JobRun) -> Vec<&str> {
    run.input
        .as_ref()
        .map(recorded_task_ids)
        .unwrap_or_default()
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
        source_run_id: None,
        step_id: step.id.clone(),
        step_index: step.index,
        activity: step.activity.to_string(),
    }
}

/// The inherited checkpoint must retain both its logical index and its
/// named pipeline entry, including the exact pointer when terminal compaction
/// moved the indexed bytes into the pipeline.
fn inherited_checkpoint_matches(
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

fn recorded_provenance(run: &JobRun, step: &DeliveryStep) -> DeliveryEvidenceProvenance {
    DeliveryEvidenceProvenance {
        source_run_id: Some(run.run_id.clone()),
        ..provenance(step)
    }
}

fn state_belongs_to(state: &PipelineState, run: &JobRun) -> bool {
    state.run_id == run.run_id.as_str() && state.job_id == run.job_id.as_str()
}

/// Execution indices are tail-local after resume; identity and success are
/// what bind the producing record to the logical definition/checkpoint.
fn executed_step(run: &JobRun, step: &DeliveryStep) -> bool {
    run.steps
        .iter()
        .rev()
        .find(|record| record.target_id == step.id.as_str())
        .is_some_and(|record| {
            record.target_type == JobTargetType::Activity && record.state == JobRunState::Success
        })
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
