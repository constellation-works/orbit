//! [ORB-10470] Resume planning: retry-lineage ownership + checkpoint reuse.
//!
//! A resumed run is not a fresh submission. It re-enters a workflow that a
//! previous attempt already admitted, in a worktree that attempt created,
//! against tasks that attempt stamped with its own `job_run_id`. Two durable
//! facts therefore have to be reconciled *before* the resumed run reaches its
//! delivery tail (F2026-07-121 / F2026-07-122):
//!
//! 1. **Blocked tasks.** A terminal run failure blocks every coupled task
//!    (`runtime::task::block_on_run_failure`), and `blocked` is not in the
//!    workflow-admission allowlist. If the resumed run replays
//!    `worktree_setup`, admission rejects the very task the resume exists to
//!    recover — a catch-22.
//! 2. **Ownership drift.** When checkpoints *are* reused, `worktree_setup` is
//!    skipped, so nothing re-claims the task. Downstream delivery steps keep
//!    consuming the checkpointed `steps.<worktree>.output.job_run_id` as their
//!    batch id, while the task record may have been re-stamped by an
//!    intervening failed attempt. `load_handoff_context` then fails closed with
//!    "task ... no longer belongs to job run ...".
//!
//! Both are repaired by reconciling against the run's **explicit retry
//! lineage** — the source run, its `retry_source_run_id` ancestors, and the
//! runs descended from them — and never against an unrelated run. A task
//! stamped by a run outside that lineage is left exactly as it is, so the
//! ownership check in `load_handoff_context` keeps its full strength.

use std::collections::BTreeSet;
use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::workflow::activity_job::run_input_declares_trusted_host;
use orbit_types::workflow::{JobRun, JobRunState, PipelineState};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::job::pipeline::{PipelineInvokeResult, PipelineSubmission};
use crate::application::job::{RunOwnerLiveness, run_owner_liveness};

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
    pub(crate) job_path: PathBuf,
    pub(crate) input: Value,
    pub(crate) attempt: u32,
    /// Source checkpoints to seed the resumed run with, when the source has at
    /// least one successful top-level step. `None` degrades to a full replay.
    pub(crate) resume_state: Option<PipelineState>,
    /// The source run, its retry ancestors, and their descendants.
    pub(crate) lineage: BTreeSet<String>,
    /// The batch id the reused checkpoints will keep handing to delivery steps.
    /// `None` when nothing is reused (`worktree_setup` re-runs and re-claims).
    pub(crate) checkpoint_batch_id: Option<String>,
}

impl OrbitRuntime {
    /// Resolve the source run, its checkpoints, and its retry lineage.
    ///
    /// Shared by both resume surfaces: the blocking CLI path
    /// (`resume_job_run`) and the asynchronous submission path
    /// (`submit_resume_run`), so they cannot drift on which runs are resumable
    /// or which checkpoints are reused.
    pub(crate) fn plan_job_run_resume(
        &self,
        source_run_id: &str,
    ) -> Result<ResumePlan, OrbitError> {
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
        // Both resume surfaces funnel through this planner (`orbit job resume`
        // and the CLI/dashboard/`workflow_tools` paths reaching
        // `submit_resume_run`), so re-verifying here covers all of them.
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
        let (job_path, _) = self.load_v2_job_asset_by_name(&source.job_id)?;
        let lineage = self.resume_lineage_run_ids(&source)?;
        let checkpoint_batch_id = resume_state.as_ref().and_then(checkpoint_ownership_id);
        let attempt = source.attempt.saturating_add(1);

        Ok(ResumePlan {
            source,
            job_path,
            input,
            attempt,
            resume_state,
            lineage,
            checkpoint_batch_id,
        })
    }

    /// The run ids that make up this resume's retry lineage: the source, every
    /// `retry_source_run_id` ancestor, and every run descended from one of
    /// them. Descendants matter because a task is commonly re-stamped by a
    /// *later* short-lived attempt (F2026-07-121: the task ended up owned by
    /// `jrun-…-2343`, a grandchild of the run being resumed).
    fn resume_lineage_run_ids(&self, source: &JobRun) -> Result<BTreeSet<String>, OrbitError> {
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

        Ok(lineage)
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
            let owned = self.list_tasks_filtered(
                None,
                None,
                None,
                Some(lineage_run_id.as_str()),
                None,
                None,
            )?;
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
                    plan.checkpoint_batch_id.as_deref(),
                    &plan.source.run_id,
                    resumed_run_id,
                ) {
                    Ok(Some(reclaimed)) => reconciled.push(reclaimed.id),
                    Ok(None) => {}
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
    /// The non-blocking counterpart to
    /// [`OrbitRuntime::resume_job_run`](crate::OrbitRuntime::resume_job_run):
    /// it persists the resumed run (seeded with the source's checkpoints),
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
    pub fn submit_resume_run(
        &self,
        source_run_id: &str,
        actor: Option<&str>,
        claim_token: Option<&str>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        self.require_workspace_claim("orbit.workflow.run.resume", claim_token)?;
        let plan = self.plan_job_run_resume(source_run_id)?;
        let job_id = plan.source.job_id.clone();
        self.submit_persisted_pipeline_run(PipelineSubmission {
            resume: Some(&plan),
            ..PipelineSubmission::catalog(&job_id, plan.input.clone(), actor)
        })
    }
}
