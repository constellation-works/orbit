//! A run's lifecycle writes: start authority, owner claims, steps, crew,
//! terminal transitions, timing repair and removal. Each rule reads the run
//! and writes the result inside one `Store::write` transaction.

use chrono::{DateTime, Utc};
use orbit_common::process::build::current_orbit_build;
use orbit_common::process::identity::process_start_identity_token;
use orbit_common::security::redaction::redact_all;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::contract::{JobRunFinalization, JobRunStepParams, PendingJobRunCancellation};
use orbit_types::identity::Crew;
use orbit_types::workflow::{
    JobRun, JobRunStartOutcome, JobRunState, JobRunStep, RunEvent, StepFailureCode,
};

use super::JobRuns;
use super::state::compact_finished_state;

impl JobRuns {
    /// Read-modify-write a run row inside one transaction. `false` when the
    /// run does not exist.
    fn update_run(
        &self,
        run_id: &str,
        update: impl FnOnce(&mut JobRun) -> Result<(), OrbitError>,
    ) -> Result<bool, OrbitError> {
        self.store.write(|tx| {
            let runs = tx.job_run();
            let Some(mut run) = runs.get(&self.workspace_id, run_id)? else {
                return Ok(false);
            };
            update(&mut run)?;
            runs.put(&self.workspace_id, &run, None)?;
            Ok(true)
        })
    }

    /// [`Self::update_run`] for a terminal write: when `update` returns
    /// `true` the run just finished, and the same transaction compacts its
    /// pipeline state for that outcome, so no reader ever sees a finished run
    /// that still carries resume-only state.
    fn update_run_and_state(
        &self,
        run_id: &str,
        update: impl FnOnce(&mut JobRun) -> Result<bool, OrbitError>,
    ) -> Result<bool, OrbitError> {
        self.store.write(|tx| {
            let runs = tx.job_run();
            let Some(mut run) = runs.get(&self.workspace_id, run_id)? else {
                return Ok(false);
            };
            if update(&mut run)? {
                runs.put(&self.workspace_id, &run, None)?;
                compact_finished_state(&runs, &self.workspace_id, run_id, run.state)?;
            }
            Ok(true)
        })
    }

    /// [ORB-10965] Apply a `Start` event to a run, atomically and
    /// idempotently: the single arbiter of job-run start authority.
    ///
    /// Scheduling is at-least-once, so exactly one caller can observe
    /// [`JobRunStartOutcome::Started`]. A redelivery from the owner already
    /// recorded on the run is [`JobRunStartOutcome::AlreadyStarted`] and
    /// writes nothing. A delivery from a different owner loses to the
    /// incumbent with [`OrbitError::JobRunStartConflict`]. A `Start` from any
    /// state other than `pending`, `running` or terminal fails with
    /// [`OrbitError::JobRunStateTransition`].
    pub fn mark_job_run_running(
        &self,
        run_id: &str,
        started_at: DateTime<Utc>,
        pid: u32,
    ) -> Result<JobRunStartOutcome, OrbitError> {
        let pid_start_time = process_start_identity_token(pid);
        self.store.write(|tx| {
            let runs = tx.job_run();
            let Some(mut run) = runs.get(&self.workspace_id, run_id)? else {
                return Ok(JobRunStartOutcome::NotFound);
            };

            // A run that already left `pending` was started by someone.
            // Duplicate at-least-once delivery from that same owner is a
            // no-op; anyone else has lost the race to the incumbent.
            if run.state == JobRunState::Running || run.state.is_terminal() {
                if run.is_owned_by(pid, pid_start_time.as_deref()) {
                    return Ok(JobRunStartOutcome::AlreadyStarted);
                }
                return Err(OrbitError::JobRunStartConflict(format!(
                    "run '{}' is already {} under owner pid {} (started at {}); \
                     the start attempt from pid {} has no execution authority",
                    run_id,
                    run.state,
                    run.pid
                        .map_or_else(|| "unknown".to_string(), |owner| owner.to_string()),
                    run.started_at
                        .map_or_else(|| "unknown".to_string(), |at| at.to_rfc3339()),
                    pid,
                )));
            }

            run.state = run.state.try_transition(RunEvent::Start)?;
            // Admission can be performed by a different binary than the
            // worker. Only the winning first Start records the executing
            // build; duplicate starts and subsequent updates preserve it.
            runs.put_build(&self.workspace_id, run_id, &current_orbit_build())?;
            run.started_at = Some(started_at);
            run.pid = Some(pid);
            run.pid_start_time = pid_start_time;
            runs.put(&self.workspace_id, &run, None)?;
            Ok(JobRunStartOutcome::Started)
        })
    }

    /// [ORB-10070] Record `pid` (and its start-time identity token) as the
    /// owner of a still-`pending` run so orphan reconciliation can tell a
    /// queued run with a live worker from one whose worker died. `false`
    /// without writing when the run is missing or no longer pending.
    pub fn claim_pending_job_run_owner(&self, run_id: &str, pid: u32) -> Result<bool, OrbitError> {
        let mut claimed = false;
        let found = self.update_run(run_id, |run| {
            if run.state != JobRunState::Pending {
                return Ok(());
            }
            run.pid = Some(pid);
            run.pid_start_time = process_start_identity_token(pid);
            claimed = true;
            Ok(())
        })?;
        Ok(found && claimed)
    }

    /// Record one step of `run_id`. `false` when the run does not exist.
    pub fn complete_job_run_step(
        &self,
        run_id: &str,
        params: &JobRunStepParams,
    ) -> Result<bool, OrbitError> {
        self.store.write(|tx| {
            let runs = tx.job_run();
            if !runs.exists(&self.workspace_id, run_id)? {
                return Ok(false);
            }
            params.state.validate_step_state()?;
            let step = JobRunStep {
                lock_wait_ms: params.lock_wait_ms,
                step_index: params.step_index as u32,
                target_type: params.target_type,
                target_id: params.target_id.clone(),
                started_at: Some(params.started_at),
                finished_at: Some(params.finished_at),
                duration_ms: params.duration_ms,
                exit_code: params.exit_code,
                agent_response_json: params.agent_response_json.clone(),
                state: params.state,
                error_code: params.error_code.clone(),
                // The store's `error_code` column is the typed code's
                // storage; parse it into its typed form at this boundary.
                failure_code: params.error_code.as_deref().map(StepFailureCode::parse),
                error_message: params.error_message.as_deref().map(redact_all),
            };
            runs.put_step(&self.workspace_id, run_id, &step)?;
            Ok(true)
        })
    }

    pub fn record_job_run_crew(&self, run_id: &str, crew: &Crew) -> Result<bool, OrbitError> {
        self.update_run(run_id, |run| {
            run.resolved_crew = Some(crew.name.clone());
            run.crew_model = Some(crew.assignment.model.clone());
            Ok(())
        })
    }

    /// Finalize a run and report the decision made with the state write in
    /// one transaction. Callers with side effects must use this outcome
    /// rather than a state read taken before finalization.
    pub fn finalize_job_run_with_outcome(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
    ) -> Result<JobRunFinalization, OrbitError> {
        let mut outcome = JobRunFinalization::Missing;
        self.update_run_and_state(run_id, |run| {
            if run.state.is_terminal() {
                outcome = JobRunFinalization::AlreadyTerminal(run.state);
                return Ok(false);
            }
            let event = match state {
                JobRunState::Success => RunEvent::Complete,
                JobRunState::Failed => RunEvent::Fail,
                JobRunState::Timeout => RunEvent::Timeout,
                JobRunState::Cancelled => RunEvent::Cancel,
                JobRunState::Interrupted => RunEvent::Interrupt,
                JobRunState::Held => RunEvent::Hold,
                other => {
                    return Err(OrbitError::JobRunStateTransition(format!(
                        "cannot finalize to non-terminal state: {other}"
                    )));
                }
            };
            run.state = run.state.try_transition(event)?;
            run.finished_at = Some(finished_at);
            run.duration_ms = duration_ms;
            outcome = JobRunFinalization::Finalized;
            Ok(true)
        })?;
        Ok(outcome)
    }

    /// `true` means the run exists, including a replay of its terminal
    /// write.
    pub fn finalize_job_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError> {
        Ok(!matches!(
            self.finalize_job_run_with_outcome(run_id, state, finished_at, duration_ms)?,
            JobRunFinalization::Missing
        ))
    }

    /// Cancel a run only while it is still `pending`: the state check and
    /// the terminal write share one transaction, as
    /// [`Self::mark_job_run_running`]'s do, so a run either starts or is
    /// cancelled here, never both. A run that already started or ended is
    /// left untouched and reported as
    /// [`PendingJobRunCancellation::NotPending`].
    pub fn cancel_pending_job_run(
        &self,
        run_id: &str,
        finished_at: DateTime<Utc>,
    ) -> Result<PendingJobRunCancellation, OrbitError> {
        let mut outcome = PendingJobRunCancellation::Missing;
        self.update_run_and_state(run_id, |run| {
            if run.state != JobRunState::Pending {
                outcome = PendingJobRunCancellation::NotPending(run.state);
                return Ok(false);
            }
            run.state = run.state.try_transition(RunEvent::Cancel)?;
            run.finished_at = Some(finished_at);
            run.duration_ms = None;
            outcome = PendingJobRunCancellation::Cancelled;
            Ok(true)
        })?;
        Ok(outcome)
    }

    /// Fill a terminal run's missing `finished_at` and `duration_ms`. `true`
    /// only when a field changed.
    pub fn repair_terminal_job_run_timing(
        &self,
        run_id: &str,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError> {
        let mut changed = false;
        let found = self.update_run(run_id, |run| {
            if !run.state.is_terminal() {
                return Ok(());
            }
            if run.finished_at.is_none() {
                run.finished_at = Some(finished_at);
                changed = true;
            }
            if run.duration_ms.is_none() {
                run.duration_ms = duration_ms;
                changed = true;
            }
            Ok(())
        })?;
        Ok(found && changed)
    }

    /// Remove a run, returning its job id; not-found when there is none.
    pub fn archive_job_run(&self, run_id: &str) -> Result<String, OrbitError> {
        self.remove_run(run_id)
    }

    /// Remove a run, returning its job id; not-found when there is none.
    pub fn delete_job_run(&self, run_id: &str) -> Result<String, OrbitError> {
        self.remove_run(run_id)
    }

    fn remove_run(&self, run_id: &str) -> Result<String, OrbitError> {
        self.store.write(|tx| {
            let runs = tx.job_run();
            let run = runs
                .get(&self.workspace_id, run_id)?
                .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
            runs.delete(&self.workspace_id, run_id)?;
            Ok(run.job_id)
        })
    }
}
