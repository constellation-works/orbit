use orbit_common::OrbitError;

use super::ResumePlan;
use crate::OrbitRuntime;
use crate::application::job::pipeline::{
    PipelineInvokeResult, PipelineSubmission, SubmittedDefinition,
};

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
        self.submit_planned_resume(&plan, actor)
    }

    /// Submit a drain's out-of-run auto-fix of the pull request
    /// `source_run_id` opened: the resume
    /// [`Self::plan_completion_repair_resume`] plans, through the same
    /// claim gate, lineage guard and ownership reconciliation as
    /// [`Self::submit_resume_run`].
    pub(crate) fn submit_completion_repair_run(
        &self,
        source_run_id: &str,
        repair: &serde_json::Value,
        actor: Option<&str>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        self.require_workspace_claim("orbit.workflow.run.resume", None)?;
        let plan = self.plan_completion_repair_resume(source_run_id, repair)?;
        self.submit_planned_resume(&plan, actor)
    }

    /// Submit a resume `plan` made under the workspace claim.
    fn submit_planned_resume(
        &self,
        plan: &ResumePlan,
        actor: Option<&str>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        // Before anything is inserted or any task is readmitted.
        plan.refuse_missing_reused_worktree()?;
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
                resume: Some(plan),
                ..PipelineSubmission::catalog(&job_id, input, actor)
            }),
            None => self.submit_persisted_pipeline_run(PipelineSubmission {
                resume: Some(plan),
                ..PipelineSubmission::catalog(&job_id, input, actor)
            }),
        }
    }
}
