//! Pipeline-state writes: replace, initialize, read-modify-write, parked-run
//! adoption and the terminal compaction a finishing write applies.

use chrono::{DateTime, Utc};
use orbit_common::process::identity::process_start_identity_token;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::{JobRunRepository, Tx};
use orbit_types::workflow::{
    AUTO_TASK_CHECKPOINT_VERSION, JobRunState, PipelineState, RunStateUpdate,
};

use super::JobRuns;

impl JobRuns {
    /// Replace a run's pipeline state. Not-found when the run does not exist.
    ///
    /// A checkpoint a newer binary wrote is never overwritten [ORB-20158],
    /// and a snapshot written after the run finished is compacted for that
    /// outcome, so it cannot bring back what finalization dropped.
    pub fn write_run_state(&self, run_id: &str, state: &PipelineState) -> Result<(), OrbitError> {
        state
            .checkpoint_write_supported()
            .map_err(OrbitError::Store)?;
        self.store.write(|tx| {
            let runs = tx.job_run();
            if let Some(raw) = runs.get_pipeline_state_json(&self.workspace_id, run_id)? {
                refuse_unsupported_stored_checkpoint(&raw)?;
            }
            let compacted = runs
                .get_state(&self.workspace_id, run_id)?
                .and_then(|run_state| {
                    let mut compacted = state.clone();
                    compacted
                        .compact_for_terminal(run_state)
                        .then_some(compacted)
                });
            let state_json = serialize(compacted.as_ref().unwrap_or(state))?;
            if !runs.put_pipeline_state_json(&self.workspace_id, run_id, &state_json)? {
                return Err(OrbitError::not_found(
                    NotFoundKind::JobRun,
                    run_id.to_string(),
                ));
            }
            Ok(())
        })
    }

    /// Initialize an existing run's pipeline state only while it is absent.
    /// `false` if another writer already supplied state, preserving that
    /// document so the caller can apply its change with
    /// [`Self::update_run_state`]. A missing run is not-found.
    pub fn initialize_run_state(
        &self,
        run_id: &str,
        state: &PipelineState,
    ) -> Result<bool, OrbitError> {
        let state_json = serialize(state)?;
        self.store.write(|tx| {
            let runs = tx.job_run();
            let inserted =
                runs.put_pipeline_state_json_if_absent(&self.workspace_id, run_id, &state_json)?;
            if !inserted && !runs.exists(&self.workspace_id, run_id)? {
                return Err(OrbitError::not_found(
                    NotFoundKind::JobRun,
                    run_id.to_string(),
                ));
            }
            Ok(inserted)
        })
    }

    /// [ORB-11253] Read-modify-write a run's pipeline state in one
    /// transaction.
    ///
    /// The run's state is one document that the engine (checkpoints, child
    /// dispatches) and operator run controls both mutate, so two interleaved
    /// read-modify-write cycles would drop whichever change landed in
    /// between. `update` also receives the run's current [`JobRunState`], so
    /// a caller that must not mutate a finished run can refuse inside the
    /// transaction. An `Err` from `update` rolls back, leaving the stored
    /// state exactly as it was. A finished run's result stays compacted
    /// (`PipelineState::compact_for_terminal`), so a late checkpoint cannot
    /// restore its resume-only maps.
    pub fn update_run_state(
        &self,
        run_id: &str,
        update: &mut dyn FnMut(JobRunState, &mut PipelineState) -> Result<(), OrbitError>,
    ) -> Result<RunStateUpdate, OrbitError> {
        self.store.write(|tx| {
            let runs = tx.job_run();
            let Some((state, raw_pipeline_state)) =
                runs.get_with_pipeline_state(&self.workspace_id, run_id)?
            else {
                return Ok(RunStateUpdate::NotFound);
            };
            let Some(raw_pipeline_state) = raw_pipeline_state else {
                return Ok(RunStateUpdate::NoState);
            };
            refuse_unsupported_stored_checkpoint(&raw_pipeline_state)?;
            let mut pipeline_state = parse(&raw_pipeline_state)?;
            update(state, &mut pipeline_state)?;
            pipeline_state
                .checkpoint_write_supported()
                .map_err(OrbitError::Store)?;
            pipeline_state.compact_for_terminal(state);
            runs.put_pipeline_state_json(&self.workspace_id, run_id, &serialize(&pipeline_state)?)?;
            Ok(RunStateUpdate::Updated)
        })
    }

    /// [ORB-20158] Resume a deliberately parked `running` run under `pid`.
    ///
    /// The run must still be `running` and its pipeline state must carry the
    /// parked continuation recorded at `parked_at`. The owner becomes `pid`
    /// and the park is cleared in one transaction, so exactly one worker
    /// resumes it. `false` when the run is missing, no longer running, not
    /// parked, or parked again under another mark.
    pub fn adopt_parked_job_run(
        &self,
        run_id: &str,
        parked_at: DateTime<Utc>,
        pid: u32,
    ) -> Result<bool, OrbitError> {
        let pid_start_time = process_start_identity_token(pid);
        self.store.write(|tx| {
            let runs = tx.job_run();
            let Some(mut run) = runs.get(&self.workspace_id, run_id)? else {
                return Ok(false);
            };
            if run.state != JobRunState::Running {
                return Ok(false);
            }
            let Some(raw) = runs.get_pipeline_state_json(&self.workspace_id, run_id)? else {
                return Ok(false);
            };
            refuse_unsupported_stored_checkpoint(&raw)?;
            let mut state = parse(&raw)?;
            let Some(checkpoint) = state.auto_task.as_mut() else {
                return Ok(false);
            };
            if checkpoint.parked.as_ref().map(|park| park.parked_at) != Some(parked_at) {
                return Ok(false);
            }
            checkpoint.parked = None;
            state.updated_at = Utc::now();
            runs.put_pipeline_state_json(&self.workspace_id, run_id, &serialize(&state)?)?;
            run.pid = Some(pid);
            run.pid_start_time = pid_start_time;
            runs.put(&self.workspace_id, &run, None)?;
            Ok(true)
        })
    }
}

fn serialize(state: &PipelineState) -> Result<String, OrbitError> {
    serde_json::to_string(state)
        .map_err(|e| OrbitError::Store(format!("serialize pipeline state: {e}")))
}

fn parse(raw: &str) -> Result<PipelineState, OrbitError> {
    serde_json::from_str(raw)
        .map_err(|e| OrbitError::Store(format!("invalid pipeline_state_json: {e}")))
}

/// [ORB-14587] Drop the resume-only maps of a run that just finished
/// `run_state`, inside the transaction that finished it. Best-effort: an
/// unreadable checkpoint is left as it is rather than failing the terminal
/// write.
pub(super) fn compact_finished_state(
    runs: &JobRunRepository<'_, Tx<'_>>,
    workspace_id: &str,
    run_id: &str,
    run_state: JobRunState,
) -> Result<(), OrbitError> {
    let Some(raw) = runs.get_pipeline_state_json(workspace_id, run_id)? else {
        return Ok(());
    };
    if refuse_unsupported_stored_checkpoint(&raw).is_err() {
        return Ok(());
    }
    let mut state = match serde_json::from_str::<PipelineState>(&raw) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!(
                run_id,
                %error,
                "finished run's pipeline state is unreadable; left uncompacted"
            );
            return Ok(());
        }
    };
    if !state.compact_for_terminal(run_state) {
        return Ok(());
    }
    runs.put_pipeline_state_json(workspace_id, run_id, &serialize(&state)?)
        .map(|_| ())
}

/// [ORB-20158] Refuse a stored state whose auto-task checkpoint a newer
/// binary wrote: rewriting it through this binary's shape could drop pending
/// bindings, receipts or the parked continuation. Read before deserializing,
/// so the refusal does not depend on the newer shape still parsing.
fn refuse_unsupported_stored_checkpoint(raw: &str) -> Result<(), OrbitError> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(());
    };
    let Some(version) = value
        .get("auto_task")
        .and_then(|checkpoint| checkpoint.get("version"))
    else {
        return Ok(());
    };
    match version.as_u64() {
        Some(version) if version <= u64::from(AUTO_TASK_CHECKPOINT_VERSION) => Ok(()),
        _ => Err(OrbitError::Store(format!(
            "auto-task checkpoint version {version} is newer than the supported \
             {AUTO_TASK_CHECKPOINT_VERSION}; this Orbit refuses to rewrite the run state"
        ))),
    }
}
