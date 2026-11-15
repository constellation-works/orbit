//! Query, list, show, and history methods for job runs: reconciling reads
//! for operators and observation-only reads for no-mutation callers.

use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::InvocationQuery;
use orbit_store::contracts::JobRunQuery;
use orbit_types::workflow::JobRun;

use crate::OrbitRuntime;
use crate::application::job::JobDrainLeafPort;

use super::projection::ActivityInvocationEvidence;
use super::reconcile::ReconcilePass;
use super::types::JobRunListParams;

impl OrbitRuntime {
    pub fn job_history(&self, job_id: &str) -> Result<Vec<JobRun>, OrbitError> {
        let mut pass = ReconcilePass::default();
        self.reconcile_stale_job_runs_with_pass(Some(job_id), &mut pass)?;
        match self.load_v2_job_asset_by_name(job_id) {
            Ok(_) => self.list_reconciled_job_history_backend(job_id, &mut pass),
            Err(error) => {
                let runs = self.list_reconciled_job_history_backend(job_id, &mut pass)?;
                if runs.is_empty() {
                    Err(error)
                } else {
                    Ok(runs)
                }
            }
        }
    }

    pub fn list_job_runs(&self, params: JobRunListParams) -> Result<Vec<JobRun>, OrbitError> {
        let mut pass = ReconcilePass::default();
        self.reconcile_stale_job_runs_with_pass(params.job_id.as_deref(), &mut pass)?;
        self.ensure_listable_job(params.job_id.as_deref())?;

        let query = job_run_query(params);
        let runs = self.list_job_runs_filtered_backend(&query)?;
        if self.reconcile_job_run_records_with_pass(&runs, &mut pass)? > 0 {
            self.list_job_runs_filtered_backend(&query)
        } else {
            Ok(runs)
        }
    }

    pub fn show_job_run(&self, run_id: &str) -> Result<JobRun, OrbitError> {
        let run = self.show_job_run_without_drain_leaf_settlement(run_id)?;
        // [ORB-23198] A drain's leaf can end after the drain's last admission
        // pass; this reconciling read records it terminal on the dispatch.
        self.record_finished_drain_leaves(&run);
        Ok(run)
    }

    /// Show a run after stale-run reconciliation without settling its drain
    /// leaves. Read projections that separately display live child outcomes
    /// can use this to keep the parent's dispatch checkpoint unchanged.
    pub fn show_job_run_without_drain_leaf_settlement(
        &self,
        run_id: &str,
    ) -> Result<JobRun, OrbitError> {
        let run = self.show_job_run_observed(run_id)?;
        Ok(if self.reconcile_stale_job_run(&run)? {
            self.show_job_run_observed(run_id)?
        } else {
            run
        })
    }

    /// The provider/model evidence each of a run's activities recorded, for
    /// [`super::job_run_to_json_with_activity_provenance`].
    ///
    /// Evidence is one row per completed invocation: its agent/model is the
    /// provider-reported identity stored at completion (which wins when the
    /// provider reports a different model than requested), while its crew and
    /// reasoning effort are the dispatch identity recorded at launch. A launch
    /// without a completed invocation is not evidence: it leaves the activity
    /// `unavailable`, never `recorded`. Old completion rows have no dispatch
    /// record. Keep their effort unknown, never reconstruct it from audit or
    /// current crew settings.
    pub fn activity_invocation_evidence(
        &self,
        run_id: &str,
    ) -> Result<Vec<ActivityInvocationEvidence>, OrbitError> {
        Ok(self
            .invocation_records(InvocationQuery {
                job_run_id: Some(run_id.to_string()),
                limit: 100_000,
                skip_tool_calls: true,
                ..InvocationQuery::default()
            })?
            .into_iter()
            .map(|record| ActivityInvocationEvidence {
                activity_id: record.activity_id,
                provider: record.provider.unwrap_or(record.agent),
                model: record.model,
                execution: record.execution,
            })
            .collect())
    }

    /// [`Self::list_job_runs`] without its stale-run reconciliation: the
    /// records exactly as stored. Reconciliation can finalize an orphaned
    /// `pending`/`running` run as `interrupted` and release its task
    /// reservations, so a reader bound to a no-mutation contract (the
    /// run-failure scan) must list through this instead [ORB-12941].
    pub fn list_job_runs_observed(
        &self,
        params: JobRunListParams,
    ) -> Result<Vec<JobRun>, OrbitError> {
        self.ensure_listable_job(params.job_id.as_deref())?;
        self.list_job_runs_filtered_backend(&job_run_query(params))
    }

    /// [`Self::count_job_runs`] without its stale-run reconciliation, for the
    /// same no-mutation readers as [`Self::list_job_runs_observed`].
    pub fn count_job_runs_observed(&self, params: JobRunListParams) -> Result<u64, OrbitError> {
        self.stores()
            .jobs()
            .count_job_runs_filtered(&job_run_query(params))
    }

    /// [`Self::show_job_run`] without its stale-run reconciliation; see
    /// [`Self::list_job_runs_observed`].
    pub fn show_job_run_observed(&self, run_id: &str) -> Result<JobRun, OrbitError> {
        self.get_job_run_backend(run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))
    }

    /// Steps-free run rows for `run_ids`, for a projection that needs state,
    /// timing and task labels only.
    ///
    /// One statement per chunk of ids, selecting no step rows, with `input`
    /// limited to the task keys the run list labels from. Missing ids are
    /// omitted. This does not reconcile; a non-terminal child still needs
    /// [`Self::show_job_run`] so a dead leaf is not shown as running.
    pub fn get_job_runs_by_id(&self, run_ids: &[String]) -> Result<Vec<JobRun>, OrbitError> {
        self.stores().jobs().get_job_runs_by_id(run_ids)
    }

    /// A job filter must name a known job or one with recorded runs.
    fn ensure_listable_job(&self, job_id: Option<&str>) -> Result<(), OrbitError> {
        if let Some(job_id) = job_id
            && let Err(error) = self.load_v2_job_asset_by_name(job_id)
            && self.list_job_history_backend(job_id)?.is_empty()
        {
            return Err(error);
        }
        Ok(())
    }

    // History/list backends and get_job_run_backend live here; the two-pass
    // reconcile helpers that share ReconcilePass live in reconcile.rs.

    pub(super) fn list_job_history_backend(&self, job_id: &str) -> Result<Vec<JobRun>, OrbitError> {
        self.stores().jobs().list_job_runs(job_id)
    }

    pub(super) fn list_job_runs_filtered_backend(
        &self,
        query: &JobRunQuery,
    ) -> Result<Vec<JobRun>, OrbitError> {
        self.stores().jobs().list_job_runs_filtered(query)
    }

    /// How many runs match `params`, without hydrating any of them. `limit`
    /// is ignored: a count is a count.
    pub fn count_job_runs(&self, params: JobRunListParams) -> Result<u64, OrbitError> {
        self.reconcile_stale_job_runs(params.job_id.as_deref())?;
        self.stores()
            .jobs()
            .count_job_runs_filtered(&job_run_query(params))
    }

    /// Recorded wall-clock durations of every run matching `params`, for
    /// percentile baselines. `limit` is ignored.
    pub fn list_job_run_durations(&self, params: JobRunListParams) -> Result<Vec<u64>, OrbitError> {
        self.reconcile_stale_job_runs(params.job_id.as_deref())?;
        self.stores()
            .jobs()
            .list_job_run_durations_filtered(&job_run_query(params))
    }

    pub(crate) fn get_job_run_backend(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        self.stores().jobs().get_job_run(run_id)
    }

    /// Only the run's lifecycle state, for polls that must not hydrate
    /// steps, input or pipeline state.
    pub(crate) fn get_job_run_state_backend(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::JobRunState>, OrbitError> {
        self.stores().jobs().get_job_run_state(run_id)
    }
}

fn job_run_query(params: JobRunListParams) -> JobRunQuery {
    JobRunQuery {
        job_id: params.job_id,
        task_id: params.task_id,
        trigger_routine: None,
        state: params.state,
        states: params.states,
        terminal_only: params.terminal_only,
        active_only: false,
        created_since: params.since,
        recency_since: None,
        limit: params.limit,
        order_by: params.order_by,
        include_steps: true,
        input_keys: None,
    }
}
