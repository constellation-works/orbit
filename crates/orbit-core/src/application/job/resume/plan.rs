use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::activity_job::run_input_declares_trusted_host;
use orbit_types::workflow::{JobRun, JobRunState, JobV2, PipelineState};
use serde_json::Value;

use super::binding::{
    blocking_run_id, checkpoint_ownership_id, foreign_binding_message, requeuing_run_id,
    resume_binding_task_ids, review_checkpoints, task_ids_from_input,
};
use super::completion_repair::{grant_completion_repair, rewind_to_completion};
use crate::OrbitRuntime;
use crate::application::job::{RunOwnerLiveness, run_owner_liveness};
use crate::application::task::TaskRecords;
use crate::application::task::{SYSTEM_ACTOR_LABEL, TaskRecordUpdateParams};
use crate::runtime::task::resumed_task_run_id;

/// Maximum `retry_source_run_id` hops walked upward from the resume source.
/// A lineage this deep is pathological; the bound keeps a corrupted cycle from
/// turning resume planning into an unbounded scan.
const RESUME_LINEAGE_MAX_HOPS: usize = 64;

/// How many of the job's most recent runs are scanned for lineage descendants.
pub(super) const RESUME_LINEAGE_SCAN_LIMIT: usize = 500;

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
    pub(super) ancestors: BTreeSet<String>,
    /// Successful host promotion checkpoints in the reused prefix.
    pub(super) review_checkpoints: BTreeMap<String, Value>,
    /// The batch id the reused checkpoints will keep handing to delivery steps.
    /// `None` when nothing is reused (`worktree_setup` re-runs and re-claims).
    pub(crate) checkpoint_batch_id: Option<String>,
    /// Definition pinned beside the source run. `None` for a catalog-backed
    /// source, which keeps resolving `source.job_id` at admission.
    pub(crate) pinned_definition: Option<PinnedRunDefinition>,
    /// An out-of-run auto-fix: the resume reruns the source's last completion
    /// round with repair authority, and readmits a task its lineage shelved
    /// in `backlog` as well as one it blocked.
    pub(super) completion_repair: bool,
}

impl ResumePlan {
    /// Refuse a resume whose reused checkpoints name a worktree that no
    /// longer exists. Checkpoint reuse skips `worktree_setup`, so every step
    /// the resume reruns would start in that missing directory and fail,
    /// after the task was already readmitted for it. Checked before the run
    /// is inserted, so a refusal changes neither the task nor the lineage.
    pub(crate) fn refuse_missing_reused_worktree(&self) -> Result<(), OrbitError> {
        let Some(path) = self
            .resume_state
            .as_ref()
            .and_then(|state| state.pipeline.get("worktree"))
            .and_then(|worktree| worktree.get("workspace_path"))
            .and_then(Value::as_str)
        else {
            return Ok(());
        };
        if std::path::Path::new(path).is_dir() {
            return Ok(());
        }
        Err(OrbitError::JobValidation(format!(
            "job run '{}' cannot be resumed: the worktree its checkpoints reuse ({path}) no \
             longer exists, so the steps it would rerun cannot start. Nothing was changed. A \
             blocked task whose run published a pull request is requeued instead (`orbit task \
             requeue`), and its next run sets up a fresh worktree on the published branch",
            self.source.run_id
        )))
    }
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
        self.plan_resume(source_run_id, None)
    }

    /// Plan an out-of-run auto-fix of the pull request `source_run_id`
    /// opened: the ordinary resume, rewound to rerun the last completion
    /// round the source reached and everything after it, with `auto_fix`
    /// granted, the footprint deferred
    /// to the first editing agent, and `repair` recorded on the run input.
    /// The run's own `completion` grant is unchanged, so a run that never
    /// held `--complete` still never merges.
    pub(crate) fn plan_completion_repair_resume(
        &self,
        source_run_id: &str,
        repair: &Value,
    ) -> Result<ResumePlan, OrbitError> {
        self.plan_resume(source_run_id, Some(repair))
    }

    fn plan_resume(
        &self,
        source_run_id: &str,
        repair: Option<&Value>,
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
        // leaves its task's commit lease behind, and resume is the
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
        if source.input.as_ref().is_some_and(|input| {
            input
                .get(orbit_types::workflow::SOURCE_REPAIR_KEY)
                .is_some()
        }) {
            return Err(OrbitError::JobValidation("operator source-repair authority covers one attempt only; inspect and submit a new explicit recovery request".into()));
        }
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
        // [ORB-14617] A run held because the forge refused its push resumes
        // from its checkpoints. Every other hold names its own resumption
        // (an evidence hold queues a fresh review on receipt).
        let forge_held = source.state == JobRunState::Held
            && self
                .read_run_state(&source.run_id)?
                .is_some_and(|state| state.pull_drain.forge_hold.is_some());
        if !forge_held
            && !matches!(
                source.state,
                JobRunState::Interrupted | JobRunState::Failed | JobRunState::Timeout
            )
        {
            return Err(OrbitError::JobValidation(format!(
                "job run '{}' is {} — resume requires an interrupted, failed, or timed-out run, \
                 or one held because the forge refused its push",
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

        let mut input = source
            .input
            .clone()
            .unwrap_or_else(|| Value::Object(Default::default()));
        let mut resume_state = self.read_run_state(&source.run_id)?.filter(|state| {
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
        if let Some(repair) = repair {
            rewind_to_completion(&definition, resume_state.as_mut())?;
            grant_completion_repair(&mut input, repair);
        }
        // A repair resume reruns completion with auto-fix, which leaves the
        // task in review like a completion run does.
        let reaches_completion =
            repair.is_some() || input.get("completion").and_then(Value::as_str) == Some("done");
        let review_checkpoints = resume_state
            .as_ref()
            .filter(|state| {
                state.run_id == source.run_id && state.job_id == source.job_id && reaches_completion
            })
            .map(|state| review_checkpoints(&definition, state))
            .unwrap_or_default();
        let (ancestors, lineage) = self.resume_lineage_run_ids(&source)?;
        let checkpoint_batch_id = resume_state.as_ref().and_then(checkpoint_ownership_id);
        let attempt = source.attempt.saturating_add(1);
        // Before the plan exists, so neither submission path can seed
        // checkpoints or start a step for a binding this lineage does not own.
        self.refuse_foreign_resume_binding(&source, &input, resume_state.as_ref(), &lineage)?;

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
            completion_repair: repair.is_some(),
        })
    }

    /// The run ids that make up this resume's retry lineage: the source, every
    /// `retry_source_run_id` ancestor, and every run descended from one of
    /// them. Descendants matter because a task is commonly re-stamped by a
    /// *later* short-lived attempt (F2026-07-121: the task ended up owned by
    /// `jrun-…-2343`, a grandchild of the run being resumed).
    pub(crate) fn resume_lineage_run_ids(
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

    /// Refuse a resume whose targeted task is bound outside this lineage.
    ///
    /// Checkpoint reuse skips `worktree_setup`, which is the step that would
    /// notice another machine's claim and continue its candidate on a new
    /// attempt. Reusing the old checkpoints instead renders validation for the
    /// old run, and the attach guard then rejects evidence the run does not
    /// own. That rejection is unchanged and still final. This check stops the
    /// resume before a run is inserted, so no checkpoint, worktree, or
    /// validation input is copied. A lineage binding is restamped only by
    /// [`Self::reclaim_task_for_resumed_run`].
    fn refuse_foreign_resume_binding(
        &self,
        source: &JobRun,
        input: &Value,
        resume_state: Option<&PipelineState>,
        lineage: &BTreeSet<String>,
    ) -> Result<(), OrbitError> {
        let mut refused = Vec::new();
        for task_id in resume_binding_task_ids(input, resume_state) {
            let task = match self.get_task(&task_id) {
                Ok(task) => task,
                Err(OrbitError::NotFound { .. }) => continue,
                Err(error) => return Err(error),
            };
            if self.resume_binding_is_lineage(&task, lineage)? {
                continue;
            }
            refused.push(foreign_binding_message(
                &task.id,
                &source.run_id,
                task.job_run_id.as_deref(),
                task.job_run_machine.as_ref(),
            ));
        }
        if refused.is_empty() {
            return Ok(());
        }
        Err(OrbitError::JobValidation(refused.join("; ")))
    }

    /// A lineage resume may keep this binding. An absent machine is a local
    /// binding recorded before locations existed. A recorded machine must be
    /// the machine that lineage run executed on: run ids are unique only
    /// per machine, so the same id on another host is a different run.
    fn resume_binding_is_lineage(
        &self,
        task: &Task,
        lineage: &BTreeSet<String>,
    ) -> Result<bool, OrbitError> {
        let Some(run_id) = task.job_run_id.as_deref() else {
            return Ok(task.job_run_machine.is_none());
        };
        if !lineage.contains(run_id) {
            return Ok(false);
        }
        let Some(bound) = &task.job_run_machine else {
            return Ok(true);
        };
        let executed_on = self
            .get_job_run_backend(run_id)?
            .and_then(|run| run.executed_on);
        Ok(executed_on.is_some_and(|origin| origin.machine_id == bound.machine_id))
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
            // Only a completion repair readmits a task its lineage's final
            // recovery requeued to `backlog`.
            let shelved = task.status == TaskStatus::Blocked
                || (plan.completion_repair && task.status == TaskStatus::Backlog);
            if task.job_run_id.as_deref() != Some(expected_owner)
                || !(shelved || matches!(task.status, TaskStatus::InProgress | TaskStatus::Review))
            {
                return Ok(());
            }
            let history = self.get_task_history(id)?;
            let block = history.iter().rev().find(|entry| entry.to_status.is_some());
            let blocking_run = block.and_then(|entry| {
                if task.status == TaskStatus::Backlog { requeuing_run_id(entry) } else { blocking_run_id(entry) }
            });
            // A manual block or requeue is a decision, not a failed-run
            // admission to undo.
            if shelved && !blocking_run.is_some_and(|run| plan.lineage.contains(run)) {
                return Ok(());
            }
            // A completion repair reruns completion on a prefix that promoted
            // the task, whatever stage the shelving found it in.
            let checkpoint = plan.review_checkpoints.get(id).filter(|output| {
                plan.ancestors.contains(expected_owner)
                    && plan.checkpoint_batch_id.as_ref().is_some_and(|batch| plan.ancestors.contains(batch))
                    && blocking_run.is_some_and(|run| plan.ancestors.contains(run))
                    && (plan.completion_repair
                        || block.is_some_and(|entry| entry.from_status == Some(TaskStatus::Review)))
                    && (output.get("no_diff_expected").and_then(Value::as_bool) == Some(true)
                        || output.get("pr_number").and_then(Value::as_str)
                            .is_some_and(|number| task.github_pr_number() == Some(number)))
            });
            let restored = shelved.then_some(
                if checkpoint.is_some() { TaskStatus::Review } else { TaskStatus::InProgress }
            );
            let restamp = plan.checkpoint_batch_id.as_ref()
                .filter(|batch| task.job_run_id.as_ref() != Some(batch));
            let owner = plan.checkpoint_batch_id.as_deref().unwrap_or(expected_owner);
            // The batch binding is needed by handoff checks; the history entry
            // couples cleanup to this attempt without changing that binding.
            let recouple = task.status == TaskStatus::InProgress
                && resumed_task_run_id(&history, owner) != Some(resumed_run_id);
            if restored.is_none() && restamp.is_none() && !recouple {
                return Ok(());
            }
            let stage = restored.unwrap_or(task.status);
            let note = format!(
                "resume lineage reconciliation: run '{resumed_run_id}' resumes '{}'; stage={stage}; blocking_run={}; reused_promotion={}; owner_run_id={owner}{}",
                plan.source.run_id, blocking_run.unwrap_or("-"), checkpoint.is_some(),
                if plan.completion_repair { "; out_of_run_auto_fix=true" } else { "" },
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
