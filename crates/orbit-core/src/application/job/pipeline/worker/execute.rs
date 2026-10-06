//! The child process's own work: execute the run it was handed.

use super::*;

impl OrbitRuntime {
    pub fn execute_pipeline_run_worker(&self, run_id: &str) -> Result<(), OrbitError> {
        self.preflight_pipeline_worker_store()?;
        upgrade_handover::bind_worker_run(run_id);
        if upgrade_handover::adopts_run(run_id) {
            return self.adopt_pipeline_run(run_id);
        }
        // [ORB-12616] A claimed leaf is executable, but only by the worker the
        // owner's claim is bound to. The check is the trusted process worker
        // binding against the durable admission — never a run input, an
        // environment value or a caller-supplied label — so a generic worker
        // that happens to pick up the run is refused exactly as before, and a
        // worker bound to a different claim is refused too.
        if let Some(admission) = self.claimed_leaf_admission(run_id)? {
            self.authorize_claimed_leaf(run_id, admission)?;
        } else if crate::application::job::claimed::CLAIMED_LEAF_JOBS
            .contains(&self.show_job_run(run_id)?.job_id.as_str())
        {
            // A claimed definition submitted by hand has no claim to execute
            // for. Refuse here rather than after `worktree_setup` and an
            // implementation step that can never reach a handoff.
            return Err(OrbitError::JobValidation(
                "claimed leaf definitions are selected by the owner's admission, not submitted \
                 directly; this run has no claim and cannot hand off"
                    .into(),
            ));
        }

        // [ORB-10070] Claim the queued run for this worker process so orphan
        // reconciliation can tell a pending run whose worker is alive and
        // polling for its admission slot apart from one whose worker died
        // (crash, SIGKILL, host reboot). Best-effort: the run may already be
        // running/terminal, and a claim failure must never block execution.
        if let Err(error) = self
            .stores()
            .jobs()
            .claim_pending_job_run_owner(run_id, std::process::id())
        {
            tracing::warn!(
                target: "orbit.core.job_run",
                run_id,
                error = %error,
                "pipeline worker could not claim its pending run; orphan \
                 detection falls back to the unclaimed-run grace window",
            );
        }
        loop {
            let run = self.show_job_run(run_id)?;
            match run.state {
                JobRunState::Pending => {}
                JobRunState::Running
                | JobRunState::Success
                | JobRunState::Failed
                | JobRunState::Timeout
                | JobRunState::Cancelled
                | JobRunState::Interrupted
                | JobRunState::Held => return Ok(()),
                other => {
                    return Err(OrbitError::Execution(format!(
                        "pipeline worker cannot execute run '{}' from state '{}'",
                        run_id, other
                    )));
                }
            }

            let (yaml_path, spec) = self.resolve_run_definition(&run)?;
            if spec.state != JobScheduleState::Enabled {
                log_best_effort(
                    "cancel disabled job run",
                    &run.run_id,
                    self.cancel_job_run(&run.run_id),
                );
                return Err(OrbitError::InvalidInput(format!(
                    "job '{}' is disabled",
                    run.job_id
                )));
            }

            if let Err(error) = self.verify_routine_dispatch_workspace(&run) {
                self.record_routine_dispatch_workspace_mismatch(&run, &error);
                log_best_effort(
                    "cancel run after workspace mismatch",
                    &run.run_id,
                    self.cancel_job_run(&run.run_id),
                );
                return Err(error);
            }

            self.reconcile_stale_job_runs(Some(&run.job_id))?;
            let active_runs = self
                .stores()
                .jobs()
                .list_pending_or_running_job_runs(&run.job_id)?;
            if !pipeline_run_is_runnable(&active_runs, &run.run_id, spec.max_active_runs) {
                thread::sleep(Duration::from_secs(PIPELINE_WAIT_MIN_POLL_SECONDS));
                continue;
            }

            return self.execute_pipeline_run_now(&run, &yaml_path);
        }
    }

    fn execute_pipeline_run_now(&self, run: &JobRun, yaml_path: &Path) -> Result<(), OrbitError> {
        let started_at = Utc::now();
        // [ORB-10965] The state read in `execute_pipeline_run_worker` and this
        // Start are separate transactions, so a second worker handed the same
        // queued run can arrive here having also seen `pending`. The store
        // arbitrates atomically; whoever does not win execution authority
        // yields here, before any agent work.
        let outcome = match self.stores().jobs().mark_job_run_running(
            &run.run_id,
            started_at,
            std::process::id(),
        ) {
            Ok(outcome) => outcome,
            Err(OrbitError::JobRunStartConflict(diagnostic)) => {
                return self.record_deduplicated_start(run, &diagnostic);
            }
            Err(error) => return Err(error),
        };
        match outcome {
            JobRunStartOutcome::Started => {}
            JobRunStartOutcome::AlreadyStarted => {
                return self.record_deduplicated_start(
                    run,
                    "this worker process had already started the run",
                );
            }
            JobRunStartOutcome::NotFound => return Ok(()),
        }
        self.execute_started_pipeline_run(run, yaml_path, started_at, true)
    }

    pub(super) fn execute_started_pipeline_run(
        &self,
        run: &JobRun,
        yaml_path: &Path,
        started_at: chrono::DateTime<Utc>,
        announce_start: bool,
    ) -> Result<(), OrbitError> {
        let input = run
            .input
            .clone()
            .unwrap_or_else(|| Value::Object(Default::default()));
        // Once Start succeeds, every later error belongs to this run. Keep the
        // whole setup path inside the outcome finalized below so crew
        // validation, event persistence, and resume-state reads cannot escape
        // with a durable `running` projection.
        let outcome = (|| {
            self.record_run_crew_for_job(&run.run_id, &input, yaml_path)?;

            if announce_start {
                self.record_event(OrbitEvent::JobRunStarted {
                    job_id: run.job_id.clone(),
                    run_id: run.run_id.clone(),
                    attempt: run.attempt,
                })?;
            }

            // [ORB-10470] The run's own persisted checkpoints are the resume
            // cursor. A run seeded by `submit_resume_run` starts at the
            // source's first non-successful step; a run whose previous worker
            // died after checkpointing continues from where that worker
            // stopped. Reusing a checkpoint is therefore idempotent — the
            // successful steps are skipped, never re-dispatched.
            let resume = self.read_run_state(&run.run_id)?.filter(|state| {
                state
                    .step_states
                    .values()
                    .any(|step_state| *step_state == JobRunState::Success)
            });
            self.run_job_v2_from_yaml_with_run_id_and_resume(
                yaml_path,
                input.clone(),
                Some(run.run_id.clone()),
                run.retry_source_run_id.clone(),
                resume.as_ref(),
            )
        })();
        let finished_at = Utc::now();
        self.record_worker_resource_limit(run, started_at, finished_at, outcome.as_ref());
        self.finalize_v2_pipeline_run(
            run,
            &input,
            started_at,
            finished_at,
            outcome.as_ref(),
            V2RunFinalizationOptions::DETACHED_WORKER,
        )?;
        // A claimed leaf's settlement that did not reach its owner is retried
        // here, after the run is final, rather than in terminalization.
        self.retry_own_claimed_leaf_settlement(&run.run_id);
        outcome.map(|_| ())
    }
}
