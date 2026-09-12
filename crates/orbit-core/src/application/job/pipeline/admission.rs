use super::*;
use orbit_types::workflow::OPERATION_ADMISSION_KEY;

use crate::application::job::crew_pools;

/// Whether caller-shaped run input names the reserved operation-mode
/// admission key [ORB-11332].
fn run_input_declares_operation_admission(input: &Value) -> bool {
    input
        .get(OPERATION_ADMISSION_KEY)
        .is_some_and(|value| !value.is_null())
}

impl OrbitRuntime {
    /// Record the `pipeline.invoke` audit for a direct-path submission, which
    /// does not route through [`Self::submit_pipeline_run`].
    pub(super) fn record_submission_audit(
        &self,
        job_name: &str,
        input: &Value,
        actor: Option<&str>,
        result: &Result<PipelineInvokeResult, OrbitError>,
    ) -> Result<(), OrbitError> {
        self.record_pipeline_audit(
            "pipeline.invoke",
            result.as_ref().ok().map(|value| value.run_id.as_str()),
            actor,
            match result {
                Ok(_) => AuditEventStatus::Success,
                Err(_) => AuditEventStatus::Failure,
            },
            json!({
                "actor": actor,
                "job_name": job_name,
                "priority": Option::<&str>::None,
                "run_id": result.as_ref().ok().map(|value| value.run_id.clone()),
                "input_hash": input_hash(input),
            }),
            result.as_ref().err().map(|error| error.to_string()),
        )
    }
    /// Persist a pipeline run and hand it to a detached worker.
    ///
    /// `resume` distinguishes the two submission shapes: `None` is a fresh
    /// attempt with a blank pipeline; `Some(plan)` links the new run to its
    /// source, seeds it with that source's checkpoints, and reconciles the
    /// lineage's task ownership before the worker can reach `worktree_setup`.
    pub(crate) fn submit_persisted_pipeline_run(
        &self,
        submission: PipelineSubmission<'_>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        match self.submit_persisted_pipeline_run_with_admission(submission, None)? {
            ChildSubmission::Submitted(result) => Ok(result),
            ChildSubmission::Skipped(reason) => Err(OrbitError::Execution(format!(
                "unconditional pipeline submission was refused as {reason}"
            ))),
        }
    }
    pub(super) fn submit_persisted_pipeline_run_with_admission(
        &self,
        submission: PipelineSubmission<'_>,
        admission: Option<&ChildPipelineAdmission>,
    ) -> Result<ChildSubmission, OrbitError> {
        let PipelineSubmission {
            job_name,
            definition,
            input,
            resume,
            actor,
            action_key,
            trusted_host,
            operation_bound,
            trigger,
        } = submission;
        // [ORB-11354] The reserved admission key is writable by exactly one
        // caller. Refusing it here — on the single path every submission
        // surface funnels through — is what stops `orbit run job`, a resume,
        // an automation key, or a child dispatch from manufacturing an
        // unsandboxed run out of ordinary job input.
        if !trusted_host && run_input_declares_trusted_host(&input) {
            return Err(reserved_trusted_host_key_error(job_name));
        }
        // [ORB-11332] The operation-mode snapshot follows the same rule: the
        // grant-bound coordinator writes it, a resume carries its persisted
        // run input forward unchanged, and a parent-authorized child inherits
        // exactly its parent's snapshot. Any other input that names it is
        // refused rather than trusted.
        let (input, authority) = match admission {
            Some(admission) => {
                match child_admission_authority(self, &admission.parent_run_id, job_name, &input)? {
                    Some((snapshot, authority)) => {
                        let mut input = input;
                        inherit_child_admission(&mut input, &snapshot)?;
                        (input, Some(authority))
                    }
                    None => {
                        if run_input_declares_operation_admission(&input) {
                            return Err(reserved_operation_key_error(job_name));
                        }
                        (input, None)
                    }
                }
            }
            None => {
                if !operation_bound
                    && resume.is_none()
                    && run_input_declares_operation_admission(&input)
                {
                    return Err(reserved_operation_key_error(job_name));
                }
                (input, None)
            }
        };
        // [ORB-11333] The review admission follows the same discipline: a
        // child inherits its parent's snapshot, a grant-bound or ordinary
        // delivery submission captures the effective policy once, and
        // ordinary input naming the key is refused.
        let mut input = input;
        crate::application::review::install_review_admission(
            self,
            job_name,
            &mut input,
            admission.map(|admission| admission.parent_run_id.as_str()),
            resume.is_some(),
        )?;
        self.install_auto_crew_admission(
            job_name,
            &mut input,
            admission.map(|admission| admission.parent_run_id.as_str()),
            resume.is_some(),
            &mut crew_pools::random_crew_ticket,
        )?;
        let result = (|| {
            let spec = match &definition {
                SubmittedDefinition::Catalog => self.load_v2_job_asset_by_name(job_name)?.1,
                SubmittedDefinition::Snapshot { spec, .. } => (*spec).clone(),
            };
            if spec.state != JobScheduleState::Enabled {
                return Err(OrbitError::InvalidInput(format!(
                    "job '{job_name}' is disabled"
                )));
            }

            if job_name == "ci_failure_sweep_pipeline"
                && matches!(definition, SubmittedDefinition::Catalog)
            {
                self.resolve_ci_sweep_input(&spec, &mut input)?;
            }

            let submitted_at = Utc::now();
            let run = if let Some(admission) = admission {
                match self
                    .stores()
                    .jobs()
                    .admit_child_job_run(&ChildJobRunAdmissionParams {
                        parent_run_id: admission.parent_run_id.clone(),
                        parent_step_id: admission.parent_step_id.clone(),
                        job_id: job_name.to_string(),
                        action: admission.action.clone(),
                        blocking: admission.blocking,
                        attempt: 1,
                        scheduled_at: submitted_at,
                        input: Some(input.clone()),
                        authority: authority.clone(),
                    })? {
                    ChildJobRunAdmissionOutcome::Admitted(run) => *run,
                    ChildJobRunAdmissionOutcome::AdmissionsStopped => {
                        return Ok(ChildSubmission::Skipped("admissions_stopped".to_string()));
                    }
                    ChildJobRunAdmissionOutcome::Refused { reason } => {
                        return Ok(ChildSubmission::Skipped(reason));
                    }
                }
            } else if let Some(key) = action_key {
                self.stores()
                    .jobs()
                    .insert_automation_job_run(job_name, input.clone(), key)?
            } else {
                let run = self.stores().jobs().insert_job_run(
                    job_name,
                    resume.map_or(1, |plan| plan.attempt),
                    submitted_at,
                    Some(input.clone()),
                    resume.map(|plan| plan.source.run_id.clone()),
                )?;
                self.seed_v2_pipeline_run(&run, &input, resume, trigger.clone())?;
                run
            };

            let trigger = if admission.is_some() {
                JobRunTrigger::child()
            } else {
                trigger
            };
            self.record_run_trigger(&run.run_id, &trigger)?;

            // Pin the definition before the worker can exist. A direct-path
            // submission must not depend on the source file surviving
            // unchanged until the detached worker gets around to reading it.
            if let SubmittedDefinition::Snapshot { yaml, .. } = &definition
                && let Err(error) = self.write_run_definition_snapshot(&run.run_id, yaml)
            {
                let _ =
                    self.finalize_pipeline_worker_startup_failure(&run, &error.to_string(), actor);
                return Err(error);
            }

            self.reconcile_stale_job_runs(Some(job_name))?;
            let active_runs = self
                .stores()
                .jobs()
                .list_pending_or_running_job_runs(job_name)?;
            let queued = !pipeline_run_is_runnable(&active_runs, &run.run_id, spec.max_active_runs);

            // A repeated automation admission resolves the original run. Only
            // pending runs need delivery; the existing Start CAS fences workers.
            if (action_key.is_none() || run.state == JobRunState::Pending)
                && let Err(error) = self.spawn_pipeline_worker(&run.run_id, actor)
            {
                let worker_log = pipeline_worker_log_path(&self.paths().logs_dir, &run.run_id)?;
                let message = format!(
                    "pipeline worker for run '{}' could not start from registered workspace '{}': \
                     {error}; worker log: '{}'",
                    run.run_id,
                    self.paths().repo_root.display(),
                    worker_log.display(),
                );
                let _ = self.finalize_pipeline_worker_startup_failure(&run, &message, actor);
                return Err(error);
            }
            Ok(ChildSubmission::Submitted(PipelineInvokeResult {
                run_id: run.run_id,
                job_name: job_name.to_string(),
                submitted_at: submitted_at.to_rfc3339(),
                queued,
            }))
        })();

        if let Some(plan) = resume {
            self.record_pipeline_audit(
                "pipeline.resume",
                result.as_ref().ok().and_then(ChildSubmission::run_id),
                actor,
                match &result {
                    Ok(_) => AuditEventStatus::Success,
                    Err(_) => AuditEventStatus::Failure,
                },
                json!({
                    "actor": actor,
                    "job_name": job_name,
                    "source_run_id": plan.source.run_id,
                    "attempt": plan.attempt,
                    "resumed_from_checkpoints": plan.resume_state.is_some(),
                    "checkpoint_batch_id": plan.checkpoint_batch_id,
                    "run_id": result.as_ref().ok().and_then(ChildSubmission::run_id),
                }),
                result.as_ref().err().map(|error| error.to_string()),
            )?;
        }

        result
    }
    /// Durably pin a submitted run's job definition next to the run record.
    fn write_run_definition_snapshot(&self, run_id: &str, yaml: &str) -> Result<(), OrbitError> {
        let dir = self.paths().job_runs_dir.clone();
        let path = run_definition_snapshot_path(&dir, run_id)?;
        atomic_write_text(&path, yaml).map_err(|error| {
            OrbitError::Io(format!(
                "write job run definition snapshot '{}': {error}",
                path.display()
            ))
        })
    }
    /// The definition a persisted run must execute: its own snapshot when the
    /// submission pinned one, otherwise the catalog asset named by the run.
    pub(crate) fn resolve_run_definition(
        &self,
        run: &JobRun,
    ) -> Result<(PathBuf, JobV2), OrbitError> {
        let snapshot = run_definition_snapshot_path(&self.paths().job_runs_dir, &run.run_id)?;
        if !snapshot.is_file() {
            return self.load_v2_job_asset_by_name(&run.job_id);
        }
        let yaml = std::fs::read_to_string(&snapshot).map_err(|error| {
            OrbitError::InvalidInput(format!("read {}: {error}", snapshot.display()))
        })?;
        let asset = load_job_asset(&yaml).map_err(|error| {
            OrbitError::InvalidInput(format!("load {}: {error}", snapshot.display()))
        })?;
        Ok((snapshot, asset.spec))
    }
    /// [ORB-10965] Record a duplicate Start that was dropped without a second
    /// execution.
    ///
    /// Delivery is at-least-once, so losing the Start race is an expected
    /// outcome, not a fault: it is logged and audited as its own event and the
    /// worker returns successfully, leaving the run's real state to the worker
    /// that does own it.
    pub(super) fn record_deduplicated_start(
        &self,
        run: &JobRun,
        reason: &str,
    ) -> Result<(), OrbitError> {
        tracing::info!(
            target: "orbit.core.job_run",
            run_id = %run.run_id,
            job_id = %run.job_id,
            attempt = run.attempt,
            reason,
            "duplicate job run start delivery deduplicated; the incumbent \
             owner keeps execution authority",
        );
        self.record_event(OrbitEvent::JobRunStartDeduplicated {
            job_id: run.job_id.clone(),
            run_id: run.run_id.clone(),
            attempt: run.attempt,
            reason: reason.to_string(),
        })
    }
    pub(crate) fn record_run_trigger(
        &self,
        run_id: &str,
        trigger: &JobRunTrigger,
    ) -> Result<(), OrbitError> {
        let Some(mut state) = self.read_run_state(run_id)? else {
            return Ok(());
        };
        state.trigger = Some(trigger.clone());
        self.write_run_state(run_id, &state)
    }
}

pub(super) fn pipeline_run_is_runnable(
    runs: &[JobRun],
    run_id: &str,
    max_active_runs: u32,
) -> bool {
    let mut ordered = runs.to_vec();
    ordered.sort_by(|left, right| {
        left.scheduled_at
            .cmp(&right.scheduled_at)
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.run_id.cmp(&right.run_id))
    });
    ordered
        .iter()
        .take(max_active_runs.max(1) as usize)
        .any(|run| run.run_id == run_id)
}

pub(crate) fn input_hash(input: &Value) -> String {
    let encoded = serde_json::to_vec(input).unwrap_or_default();
    format!("{:x}", Sha256::digest(encoded))
}
