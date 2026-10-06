//! [ORB-10470] Resume planning: retry-lineage ownership + checkpoint reuse.
//!
//! A resumed run is not a fresh submission. It re-enters a workflow that a
//! previous attempt already admitted, in a worktree that attempt created,
//! against tasks that attempt stamped with its own `job_run_id`. Three durable
//! facts therefore have to be reconciled *before* the resumed run reaches its
//! delivery tail (F2026-07-121 / F2026-07-122):
//!
//! 1. **Blocked tasks.** A terminal run failure or interruption blocks every
//!    coupled task (`runtime::task::block_on_run_failure`; `workflow_run_failed`
//!    or `workflow_run_interrupted`). The run's own failure handoff can block
//!    the task first (`pr_failure_handoff`, `pr_conflict_blocked`,
//!    `validation_environment_blocked`, `review_gate_escalation`), and
//!    finalization then leaves that block in place because `blocked` is not
//!    blockable again. `blocked` is also outside the workflow-admission
//!    allowlist. If the resumed run replays `worktree_setup`, admission
//!    rejects the very task the resume exists to recover — a catch-22.
//!    Resume readmits a block only when that latest system entry attributes
//!    itself to this lineage. An operator block stays untouched.
//! 2. **Ownership drift.** When checkpoints *are* reused, `worktree_setup` is
//!    skipped, so nothing re-claims the task. Downstream delivery steps keep
//!    consuming the checkpointed `steps.<worktree>.output.job_run_id` as their
//!    batch id, while the task record may have been re-stamped by an
//!    intervening failed attempt. `load_handoff_context` then fails closed with
//!    "task ... no longer belongs to job run ...".
//!
//! 3. **Delivery stage.** A final-recovery escalation, or a failure-handoff
//!    block attributed to this lineage, can block a task after promotion to
//!    review. Reusing that promotion checkpoint must restore review, rather
//!    than readmitting implementation that completion skips.
//!
//! These are repaired by reconciling against the run's **explicit retry
//! lineage** — the source run, its `retry_source_run_id` ancestors, and the
//! runs descended from them — and never against an unrelated run. A task
//! stamped by a run outside that lineage is left exactly as it is, so the
//! ownership check in `load_handoff_context` keeps its full strength.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::activity_job::run_input_declares_trusted_host;
use orbit_types::workflow::{
    ActivityV2Spec, JobRun, JobRunState, JobV2, JobV2StepBody, PipelineState,
};

use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::job::pipeline::{
    PipelineInvokeResult, PipelineSubmission, SubmittedDefinition,
};
use crate::application::job::{RunOwnerLiveness, run_owner_liveness};
use crate::application::task::{SYSTEM_ACTOR_LABEL, TaskRecordUpdateParams};

/// Maximum `retry_source_run_id` hops walked upward from the resume source.
/// A lineage this deep is pathological; the bound keeps a corrupted cycle from
/// turning resume planning into an unbounded scan.
const RESUME_LINEAGE_MAX_HOPS: usize = 64;

/// How many of the job's most recent runs are scanned for lineage descendants.
const RESUME_LINEAGE_SCAN_LIMIT: usize = 500;

/// Checkpointed output fields that carry the batch/ownership identity a
/// resumed delivery tail keeps using (`worktree_setup` emits both).
const OWNERSHIP_ID_FIELDS: [&str; 2] = ["job_run_id", "batch_id"];

/// Everything `resume` resolves from the source run before a new run exists.
pub(crate) struct ResumePlan {
    pub(crate) source: JobRun,
    pub(crate) input: Value,
    pub(crate) attempt: u32,
    /// Source checkpoints to seed the resumed run with, when the source has at
    /// least one successful top-level step. `None` degrades to a full replay.
    pub(crate) resume_state: Option<PipelineState>,
    /// The source run, its retry ancestors, and their descendants.
    pub(crate) lineage: BTreeSet<String>,
    /// Ancestors only: a superseding descendant cannot donate review authority.
    ancestors: BTreeSet<String>,
    /// Successful host promotion checkpoints in the reused prefix.
    review_checkpoints: BTreeMap<String, Value>,
    /// The batch id the reused checkpoints will keep handing to delivery steps.
    /// `None` when nothing is reused (`worktree_setup` re-runs and re-claims).
    pub(crate) checkpoint_batch_id: Option<String>,
    /// Definition pinned beside the source run. `None` for a catalog-backed
    /// source, which keeps resolving `source.job_id` at admission.
    pub(crate) pinned_definition: Option<PinnedRunDefinition>,
}

/// Exact job definition a snapshot-backed run must keep across resumes.
#[derive(Clone)]
pub(crate) struct PinnedRunDefinition {
    pub(crate) spec: JobV2,
    pub(crate) yaml: String,
}

impl OrbitRuntime {
    /// Resolve the source run, its checkpoints, and its retry lineage.
    ///
    /// Shared by production detached submission and the in-process test
    /// fixtures, so admission and checkpoint rules stay aligned.
    pub(crate) fn plan_job_run_resume(
        &self,
        source_run_id: &str,
    ) -> Result<ResumePlan, OrbitError> {
        // Refuse before show_job_run can reconcile local liveness. Settled claims
        // retain immutable bindings, so even a revoked attempt cannot resume.
        let recorded = self.get_job_run_backend(source_run_id)?;
        if self
            .stores()
            .jobs()
            .local_pull_for_run(source_run_id)?
            .is_some()
        {
            return Err(OrbitError::JobValidation("claimed execution cannot use generic resume; deliberately recover and admit a new attempt".into()));
        }
        // [ORB-12575] Read the claims as an ordinary participant, not through
        // the doctor's non-repairing inspection. A worker killed mid-commit
        // leaves the partition's pending marker behind, and resume is the
        // command the operator reaches for next; it must replay that journal
        // like every other runtime read does, not refuse until something
        // unrelated happens to.
        let claims = self.resolve_execution_claims().map_err(|error| {
            let partition = self
                .workspace_id()
                .unwrap_or_else(|_| "<unresolved>".to_string());
            OrbitError::Store(format!(
                "task partition '{partition}' could not settle its execution claims before \
                 resuming '{source_run_id}': {error}. An interrupted coordination commit stays \
                 pending until its journal replays; inspect the partition with `orbit doctor`, \
                 repair it, then retry `orbit job resume {source_run_id}`"
            ))
        })?;
        if claims.iter().any(|claim| {
            claim.bound_run.as_ref().is_some_and(|run| {
                run.run_id == source_run_id
                    && recorded
                        .as_ref()
                        .and_then(|r| r.executed_on.as_ref())
                        .is_none_or(|origin| origin.machine_id == run.machine_id)
            })
        }) {
            return Err(OrbitError::JobValidation("claimed execution cannot use generic resume; deliberately recover and admit a new attempt".into()));
        }
        // `show_job_run` reconciles a stale Running owner first, so a run
        // orphaned by SIGKILL flips to Interrupted before the state guard.
        let source = self.show_job_run(source_run_id)?;
        // [ORB-11354] A resume reuses the source run's persisted input, which
        // for a trusted-host invocation would carry its admission forward into
        // a run nobody authorized now. The mode is per-invocation on purpose,
        // so resume refuses rather than re-admitting; submitting again is the
        // operator's explicit re-authorization.
        if source
            .input
            .as_ref()
            .is_some_and(orbit_types::workflow::run_input_declares_review_reconciliation)
        {
            return Err(OrbitError::JobValidation(format!(
                "job run '{source_run_id}' was an operator-admitted review reconciliation and \
                 cannot be resumed; its admission covered that attempt only. Submit its request \
                 key again with `orbit task reconcile-review submit` to admit another attempt"
            )));
        }
        if source
            .input
            .as_ref()
            .is_some_and(run_input_declares_trusted_host)
        {
            return Err(OrbitError::JobValidation(format!(
                "job run '{source_run_id}' was an operator-admitted trusted host invocation and \
                 cannot be resumed; its admission covered that invocation only. Submit a new \
                 `orbit.agent.invoke` to authorize another one"
            )));
        }
        if !matches!(
            source.state,
            JobRunState::Interrupted | JobRunState::Failed | JobRunState::Timeout
        ) {
            return Err(OrbitError::JobValidation(format!(
                "job run '{}' is {} — resume requires an interrupted, failed, or timed-out run",
                source_run_id, source.state
            )));
        }

        // [ORB-10597] For `interrupted`, a terminal state is not proof the
        // source stopped working. `interrupted` is the one resumable state
        // written by an *observer* rather than by the run itself — the orphan
        // sweep condemns a run it believes is dead, and attaches no teardown —
        // so a run condemned in error is still executing. Resuming then starts
        // a second execution against the same worktree, the same task claims,
        // and the same delivery tail as the first.
        //
        // Scoped to `interrupted` deliberately. `failed` and `timeout` are
        // self-reported: the worker writes them and then exits, so its PID is
        // routinely still alive for the moment after (and for the blocking
        // `execute_job` path the recorded owner is the caller's own process,
        // which stays alive by design). Treating those as concurrent execution
        // would refuse the most common resume there is.
        //
        // CLI, dashboard, and workflow tools all reach this planner through
        // `submit_resume_run`, so re-verifying here covers all of them.
        // Fail-safe direction is the opposite of the sweep's: refuse only on a
        // *confirmed*-alive owner, so an unprobeable one (foreign PID
        // namespace, non-Unix) does not make a legitimately dead run
        // unresumable.
        if source.state == JobRunState::Interrupted
            && run_owner_liveness(&source) == RunOwnerLiveness::Alive
        {
            return Err(OrbitError::JobValidation(format!(
                "job run '{}' is {} but its recorded worker process (pid {}) is still alive — \
                 resuming would run alongside it; stop that process or wait for the run to finish",
                source_run_id,
                source.state,
                source
                    .pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "-".to_string()),
            )));
        }

        let input = source
            .input
            .clone()
            .unwrap_or_else(|| Value::Object(Default::default()));
        let resume_state = self.read_run_state(&source.run_id)?.filter(|state| {
            state
                .step_states
                .values()
                .any(|step_state| *step_state == JobRunState::Success)
        });
        // [ORB-13559] A direct-YAML run already pinned its definition. Requiring
        // a catalog asset here would refuse a resume after the source file is
        // gone, and a later catalog submission would let a different same-name
        // asset replace that snapshot. Catalog-backed runs still resolve by name.
        let pinned_definition = self
            .read_run_definition_snapshot(&source.run_id)?
            .map(|(spec, yaml)| PinnedRunDefinition { spec, yaml });
        let mut definition = match &pinned_definition {
            Some(pinned) => pinned.spec.clone(),
            None => self.load_v2_job_asset_by_name(&source.job_id)?.1,
        };
        orbit_engine::resolve_job_catalog_refs_for_execution(
            &mut definition,
            &self
                .v2_activity_catalog()
                .map_err(|error| OrbitError::JobValidation(error.to_string()))?,
        )
        .map_err(orbit_engine::dispatch_error_to_orbit)?;
        let review_checkpoints = resume_state
            .as_ref()
            .filter(|state| {
                state.run_id == source.run_id
                    && state.job_id == source.job_id
                    && input.get("completion").and_then(Value::as_str) == Some("done")
            })
            .map(|state| review_checkpoints(&definition, state))
            .unwrap_or_default();
        let (ancestors, lineage) = self.resume_lineage_run_ids(&source)?;
        let checkpoint_batch_id = resume_state.as_ref().and_then(checkpoint_ownership_id);
        let attempt = source.attempt.saturating_add(1);

        Ok(ResumePlan {
            source,
            input,
            attempt,
            resume_state,
            lineage,
            ancestors,
            review_checkpoints,
            checkpoint_batch_id,
            pinned_definition,
        })
    }

    /// The run ids that make up this resume's retry lineage: the source, every
    /// `retry_source_run_id` ancestor, and every run descended from one of
    /// them. Descendants matter because a task is commonly re-stamped by a
    /// *later* short-lived attempt (F2026-07-121: the task ended up owned by
    /// `jrun-…-2343`, a grandchild of the run being resumed).
    fn resume_lineage_run_ids(
        &self,
        source: &JobRun,
    ) -> Result<(BTreeSet<String>, BTreeSet<String>), OrbitError> {
        let mut lineage = BTreeSet::from([source.run_id.clone()]);

        let mut cursor = source.retry_source_run_id.clone();
        for _ in 0..RESUME_LINEAGE_MAX_HOPS {
            let Some(run_id) = cursor.take() else { break };
            if !lineage.insert(run_id.clone()) {
                break;
            }
            cursor = self
                .get_job_run_backend(&run_id)?
                .and_then(|run| run.retry_source_run_id);
        }

        let ancestors = lineage.clone();
        let candidates = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some(source.job_id.clone()),
            state: None,
            terminal_only: false,
            created_since: None,
            limit: Some(RESUME_LINEAGE_SCAN_LIMIT),
            ..Default::default()
        })?;
        // Descendants can appear in any order relative to their parents, so
        // grow the set to a fixpoint rather than in a single pass.
        loop {
            let mut grew = false;
            for run in &candidates {
                if run
                    .retry_source_run_id
                    .as_deref()
                    .is_some_and(|parent| lineage.contains(parent))
                    && lineage.insert(run.run_id.clone())
                {
                    grew = true;
                }
            }
            if !grew {
                break;
            }
        }

        Ok((ancestors, lineage))
    }

    /// Re-admit and re-claim the tasks this resume owns, so the resumed run
    /// can reach its delivery tail.
    ///
    /// Scope is deliberately narrow: only tasks currently stamped with a run id
    /// from `plan.lineage`, further intersected with the run input's own
    /// `task_ids` when it names any, and never a task still owned by a
    /// *live* run in that lineage (an earlier resume that is still executing
    /// keeps its claim). Per-task write failures are logged and skipped — one
    /// task must not strand the rest of the bundle — and the downstream
    /// admission/ownership checks still fail closed on anything this pass could
    /// not repair.
    ///
    /// Idempotent: a second call against already-reconciled tasks writes
    /// nothing and returns an empty list.
    pub(crate) fn reconcile_resume_task_ownership(
        &self,
        plan: &ResumePlan,
        resumed_run_id: &str,
    ) -> Result<Vec<String>, OrbitError> {
        let scoped = task_ids_from_input(&plan.input);
        let mut visited = BTreeSet::new();
        let mut reconciled = Vec::new();

        for lineage_run_id in &plan.lineage {
            if lineage_run_id != &plan.source.run_id
                && self
                    .get_job_run_backend(lineage_run_id)?
                    .is_some_and(|run| !run.state.is_terminal())
            {
                tracing::info!(
                    target: "orbit.core.job_run",
                    run_id = resumed_run_id,
                    owner_run_id = %lineage_run_id,
                    "resume leaves tasks claimed by a still-live run in the same lineage alone",
                );
                continue;
            }
            let owned = self.list_run_tasks(lineage_run_id)?;
            for task in owned {
                if !visited.insert(task.id.clone()) {
                    continue;
                }
                if scoped
                    .as_ref()
                    .is_some_and(|task_ids| !task_ids.contains(&task.id))
                {
                    continue;
                }
                match self.reclaim_task_for_resumed_run(
                    &task.id,
                    lineage_run_id,
                    plan,
                    resumed_run_id,
                ) {
                    Ok(true) => reconciled.push(task.id),
                    Ok(false) => {}
                    Err(error) => tracing::warn!(
                        target: "orbit.core.job_run",
                        run_id = resumed_run_id,
                        source_run_id = %plan.source.run_id,
                        task_id = %task.id,
                        error = %error,
                        "resume could not reconcile task ownership; downstream admission \
                         and handoff checks stay authoritative",
                    ),
                }
            }
        }

        Ok(reconciled)
    }

    /// Recheck ownership, withdrawal and stage evidence under the task lock.
    /// History is append-only; restoration grants no completion authority.
    fn reclaim_task_for_resumed_run(
        &self,
        id: &str,
        expected_owner: &str,
        plan: &ResumePlan,
        resumed_run_id: &str,
    ) -> Result<bool, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let mut changed = false;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let task = self.get_task(id)?;
            if task.job_run_id.as_deref() != Some(expected_owner)
                || !matches!(task.status, TaskStatus::Blocked | TaskStatus::InProgress | TaskStatus::Review)
            {
                return Ok(());
            }
            let history = self.get_task_history(id)?;
            let block = history.iter().rev().find(|entry| entry.to_status.is_some());
            let blocking_run = block.and_then(blocking_run_id);
            // A manual block is a decision, not a failed-run admission to undo.
            if task.status == TaskStatus::Blocked
                && !blocking_run.is_some_and(|run| plan.lineage.contains(run))
            {
                return Ok(());
            }
            let checkpoint = plan.review_checkpoints.get(id).filter(|output| {
                plan.ancestors.contains(expected_owner)
                    && plan.checkpoint_batch_id.as_ref().is_some_and(|batch| plan.ancestors.contains(batch))
                    && blocking_run.is_some_and(|run| plan.ancestors.contains(run))
                    && block.is_some_and(|entry| entry.from_status == Some(TaskStatus::Review))
                    && (output.get("no_diff_expected").and_then(Value::as_bool) == Some(true)
                        || output.get("pr_number").and_then(Value::as_str)
                            .is_some_and(|number| task.github_pr_number() == Some(number)))
            });
            let restored = (task.status == TaskStatus::Blocked).then_some(
                if checkpoint.is_some() { TaskStatus::Review } else { TaskStatus::InProgress }
            );
            let restamp = plan.checkpoint_batch_id.as_ref()
                .filter(|batch| task.job_run_id.as_ref() != Some(batch));
            if restored.is_none() && restamp.is_none() {
                return Ok(());
            }
            let stage = restored.unwrap_or(task.status);
            let note = format!(
                "resume lineage reconciliation: run '{resumed_run_id}' resumes '{}'; stage={stage}; blocking_run={}; reused_promotion={}",
                plan.source.run_id, blocking_run.unwrap_or("-"), checkpoint.is_some(),
            );
            self.with_mutation(|| {
                let updated = self.stores().task_records().update(id, TaskRecordUpdateParams {
                    actor: SYSTEM_ACTOR_LABEL.to_string(),
                    status: restored,
                    expected_status: Some(vec![task.status]),
                    job_run_id: restamp.cloned().map(Some),
                    status_event: Some(if restored == Some(TaskStatus::Review) {
                        "resume_review_restored"
                    } else { "resume_readmitted" }.to_string()),
                    status_note: Some(note.clone()),
                    ..Default::default()
                })?;
                let event = if restored == Some(TaskStatus::InProgress) {
                    OrbitEvent::TaskStarted { id: id.to_string(), started_by: SYSTEM_ACTOR_LABEL.to_string(), approved_from_proposed: false }
                } else { OrbitEvent::TaskUpdated { id: id.to_string() } };
                Ok((updated, event))
            })?;
            changed = true;
            Ok(())
        })?;
        Ok(changed)
    }
}

/// The run a system block attributes itself to, when resume may undo it.
///
/// Workflow failure and interruption notes carry `, run_id=<id>,`. Final
/// recovery carries `run_id=` inside its escalation prefix. The run's own
/// failure handoff (`pr_failure_handoff`, `pr_conflict_blocked`,
/// `validation_environment_blocked`, `review_gate_escalation` in
/// `executor::automation::vcs::failure`) carries `: run=<id>,`. Any other
/// block, including an operator decision, returns `None` and stays put.
fn blocking_run_id(entry: &TaskHistoryEntry) -> Option<&str> {
    if entry.by != SYSTEM_ACTOR_LABEL || entry.to_status != Some(TaskStatus::Blocked) {
        return None;
    }
    let note = entry.note.as_deref()?;
    match entry.event.as_str() {
        orbit_engine::WORKFLOW_RUN_FAILED_EVENT | orbit_engine::WORKFLOW_RUN_INTERRUPTED_EVENT => {
            note.split_once(", run_id=")?.1.split(',').next()
        }
        "final_recovery_escalated" => note
            .strip_prefix("final recovery (run_id=")?
            .split_once(") escalated:")
            .map(|(id, _)| id),
        "pr_failure_handoff"
        | "pr_conflict_blocked"
        | "validation_environment_blocked"
        | "review_gate_escalation" => failure_handoff_run_id(note),
        _ => None,
    }
}

/// `run=<id>` in a failure-handoff note, bounded by the next comma or semicolon.
fn failure_handoff_run_id(note: &str) -> Option<&str> {
    let id = note
        .split_once(": run=")?
        .1
        .split([',', ';'])
        .next()?
        .trim();
    (!id.is_empty()).then_some(id)
}

/// Resolve host actions, never agent claims or a step name alone. A promotion
/// must be in the reused prefix and name the task. Later failed steps and
/// exhaustion of the job do not discard the stage already reached.
fn review_checkpoints(job: &JobV2, state: &PipelineState) -> BTreeMap<String, Value> {
    let mut promoted = BTreeMap::new();
    for (index, step) in job.steps.iter().enumerate() {
        let Ok(index) = u32::try_from(index) else {
            break;
        };
        let action = match &step.body {
            JobV2StepBody::Target(target) => match &target.spec {
                ActivityV2Spec::Deterministic(spec) => Some(spec.action.as_str()),
                _ => None,
            },
            _ => None,
        };
        let status = state.step_states.get(&index);
        if action == Some("pr_complete") && status != Some(&JobRunState::Success) {
            return promoted;
        }
        if !matches!(status, Some(JobRunState::Success | JobRunState::Skipped)) {
            break;
        }
        if action != Some("pr_promote") || status != Some(&JobRunState::Success) {
            continue;
        }
        let Some(output) = state.step_outputs.get(&index) else {
            break;
        };
        // A false `when` is checkpointed as success with null output (for
        // example the alternate no-diff promotion in the shipped pipeline).
        if output.is_null() {
            continue;
        }
        if state.pipeline.get(&step.id) != Some(output)
            || output.get("phase").and_then(Value::as_str) != Some("promote")
        {
            break;
        }
        for field in ["performed_task_ids", "reused_task_ids"] {
            for id in output
                .get(field)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                promoted.insert(id.to_string(), output.clone());
            }
        }
    }
    promoted
}

/// The batch/ownership id embedded in the earliest successful checkpoint that
/// carries one. `worktree_setup` is step 0 of every task pipeline and emits
/// both `job_run_id` and `batch_id`, so this resolves to the run that actually
/// owns the reused worktree — which is the id the resumed delivery tail keeps
/// templating into `git_push` / `pr_open` / `pr_promote`.
pub(super) fn checkpoint_ownership_id(state: &PipelineState) -> Option<String> {
    state
        .step_states
        .iter()
        .filter(|(_, step_state)| **step_state == JobRunState::Success)
        .filter_map(|(index, _)| state.step_outputs.get(index))
        .find_map(|output| {
            OWNERSHIP_ID_FIELDS.iter().find_map(|field| {
                output
                    .get(field)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
        })
}

/// Task ids the run input explicitly targets, if any. An auto-discovery run
/// names none, in which case lineage ownership is the only scope.
pub(super) fn task_ids_from_input(input: &Value) -> Option<BTreeSet<String>> {
    let ids: BTreeSet<String> = input
        .get("task_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .chain(input.get("task_id").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    (!ids.is_empty()).then_some(ids)
}

impl OrbitRuntime {
    /// [ORB-10470] Submit a resume of a terminal run as a detached run.
    ///
    /// It persists the resumed run (seeded with the source's checkpoints),
    /// reconciles the retry lineage's task ownership, spawns the detached
    /// pipeline worker, and returns the new run id as soon as the run is
    /// durable. Nothing about the resumed execution happens on the caller's
    /// thread, so run list / status / cancel stay answerable for its whole
    /// duration (F2026-07-122 defect 3) and the run is cancellable by pid like
    /// any other submitted run.
    ///
    /// [ORB-10709] Resuming creates another managed run, so it is a governed
    /// workflow operation and takes the same workspace-claim gate as
    /// [`Self::submit_ship_run`] — checked here, on the shared path, rather than
    /// in the adapters.
    ///
    /// At most one run per retry lineage is live. Every run in a lineage reuses
    /// the same checkpointed worktree and task claims, so while one is pending
    /// or running, a further resume of any member is refused with
    /// [`OrbitError::ResumeRunInFlight`] naming it. The check is part of the
    /// store insert, so concurrent requests from the dashboard, MCP, and CLI
    /// admit exactly one. Once that run is terminal, resuming is allowed again
    /// and chains from the run the caller names — its checkpoints and attempt
    /// number — not from the lineage's latest attempt.
    ///
    /// [ORB-13559] A snapshot-backed source copies its pinned definition onto
    /// the new run, so a later retry of that run keeps the same YAML. A
    /// catalog-backed source still resolves its job name from the catalog.
    pub fn submit_resume_run(
        &self,
        source_run_id: &str,
        actor: Option<&str>,
        claim_token: Option<&str>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        self.require_workspace_claim("orbit.workflow.run.resume", claim_token)?;
        let plan = self.plan_job_run_resume(source_run_id)?;
        let job_id = plan.source.job_id.clone();
        // Settle orphans first: a lineage run whose worker died (a host reboot)
        // still reads as running and would otherwise refuse the resume that
        // exists to recover it.
        self.reconcile_stale_job_runs(Some(&job_id))?;
        let pinned = plan.pinned_definition.clone();
        let input = plan.input.clone();
        match &pinned {
            Some(pinned) => self.submit_persisted_pipeline_run(PipelineSubmission {
                definition: SubmittedDefinition::Snapshot {
                    spec: &pinned.spec,
                    yaml: &pinned.yaml,
                },
                resume: Some(&plan),
                ..PipelineSubmission::catalog(&job_id, input, actor)
            }),
            None => self.submit_persisted_pipeline_run(PipelineSubmission {
                resume: Some(&plan),
                ..PipelineSubmission::catalog(&job_id, input, actor)
            }),
        }
    }

    /// [ORB-14273] Auto-resume runs interrupted by an upgrade admission or generation switch, once.
    ///
    /// Finds interrupted runs that carry the `upgrade_quiesce` error code, verifies they
    /// have no existing retry descendant (ensuring once-only resumption), filters out claimed
    /// execution and active workers, and submits a detached resume run.
    pub fn auto_resume_upgrade_interrupted_runs(&self) -> Result<Vec<String>, OrbitError> {
        self.auto_resume_upgrade_interrupted_runs_with(&mut |source_run_id| {
            self.submit_resume_run(source_run_id, Some("clock"), None)
                .map(|invoke| invoke.run_id)
        })
    }

    pub(crate) fn auto_resume_upgrade_interrupted_runs_with(
        &self,
        submit: &mut dyn FnMut(&str) -> Result<String, OrbitError>,
    ) -> Result<Vec<String>, OrbitError> {
        if self.is_write_free() {
            return Ok(Vec::new());
        }
        let query = JobRunQuery {
            state: Some(JobRunState::Interrupted),
            include_steps: true,
            ..JobRunQuery::default()
        };
        let interrupted = self.stores().jobs().list_job_runs_filtered(&query)?;
        let mut resumed = Vec::new();

        for run in interrupted {
            let is_upgrade_interrupted = run.steps.iter().any(|step| {
                step.error_code.as_deref()
                    == Some(crate::runtime::upgrade_handover::UPGRADE_QUIESCE_ERROR_CODE)
                    || step.error_message.as_deref().is_some_and(|msg| {
                        msg.contains("upgrade_quiesce") || msg.contains("upgrade admission refused")
                    })
            });
            if !is_upgrade_interrupted {
                continue;
            }

            // Once-only guard: verify the run has no retry descendant.
            let direct_retries = self.stores().jobs().job_run_retries(&run.run_id, 1)?;
            if !direct_retries.is_empty() {
                continue;
            }

            // Exclude claimed follower leaves (generic resume is refused and
            // claim recovery on the owner handles them).
            if self
                .stores()
                .jobs()
                .local_pull_for_run(&run.run_id)?
                .is_some()
            {
                tracing::debug!(
                    target: "orbit.core.sweep",
                    run_id = %run.run_id,
                    "skipping claimed follower leaf; owner claim recovery handles it",
                );
                continue;
            }
            let is_claimed_execution = self.resolve_execution_claims().is_ok_and(|claims| {
                claims.iter().any(|claim| {
                    claim
                        .bound_run
                        .as_ref()
                        .is_some_and(|bound| bound.run_id == run.run_id)
                })
            });
            if is_claimed_execution {
                tracing::debug!(
                    target: "orbit.core.sweep",
                    run_id = %run.run_id,
                    "skipping claimed execution; owner claim recovery handles it",
                );
                continue;
            }

            // A worker confirmed alive must not be resumed concurrently.
            if run_owner_liveness(&run) == RunOwnerLiveness::Alive {
                tracing::debug!(
                    target: "orbit.core.sweep",
                    run_id = %run.run_id,
                    "skipping upgrade-interrupted run whose worker process is still alive",
                );
                continue;
            }

            // Check that the run is resumable before attempting submission.
            if let Err(error) = self.plan_job_run_resume(&run.run_id) {
                tracing::debug!(
                    target: "orbit.core.sweep",
                    run_id = %run.run_id,
                    error = %error,
                    "upgrade-interrupted run is not resumable",
                );
                continue;
            }

            match submit(&run.run_id) {
                Ok(resumed_run_id) => {
                    tracing::info!(
                        target: "orbit.core.sweep",
                        source_run_id = %run.run_id,
                        resumed_run_id = %resumed_run_id,
                        "clock tick resumed upgrade-interrupted run",
                    );
                    resumed.push(resumed_run_id);
                }
                Err(error) => {
                    tracing::warn!(
                        target: "orbit.core.sweep",
                        source_run_id = %run.run_id,
                        error = %error,
                        "failed to resume upgrade-interrupted run",
                    );
                }
            }
        }

        Ok(resumed)
    }
}
