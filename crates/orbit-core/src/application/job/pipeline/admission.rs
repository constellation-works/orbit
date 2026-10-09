use super::*;

use crate::application::job::crew_pools;
use orbit_common::fs::io::open_read_only_no_follow;

impl OrbitRuntime {
    /// Record the `pipeline.invoke` audit for a direct-path submission, which
    /// does not route through [`Self::submit_pipeline_run`]. Audit failures
    /// are logged without replacing the submission outcome.
    pub(super) fn record_submission_audit(
        &self,
        job_name: &str,
        input: &Value,
        actor: Option<&str>,
        result: &Result<PipelineInvokeResult, OrbitError>,
    ) {
        log_best_effort(
            "record pipeline submission audit",
            result
                .as_ref()
                .ok()
                .map(|value| value.run_id.as_str())
                .unwrap_or_default(),
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
            ),
        );
    }
    /// Persist a pipeline run and hand it to a detached worker.
    ///
    /// A fresh attempt starts with a blank pipeline. Replay links its source
    /// while keeping that blank state; `resume: Some(plan)` instead seeds the
    /// source's checkpoints and reconciles the lineage's task ownership before
    /// the worker can reach `worktree_setup`.
    pub(crate) fn submit_persisted_pipeline_run(
        &self,
        submission: PipelineSubmission<'_>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        self.submit_keyed_pipeline_run(submission)
            .map(|(result, _)| result)
    }
    /// [`Self::submit_persisted_pipeline_run`], also reporting whether a
    /// keyed submission resolved an existing run (`true`) instead of
    /// admitting a new one.
    pub(crate) fn submit_keyed_pipeline_run(
        &self,
        submission: PipelineSubmission<'_>,
    ) -> Result<(PipelineInvokeResult, bool), OrbitError> {
        match self.submit_persisted_pipeline_run_with_admission(submission, None)? {
            ChildSubmission::Submitted(result) => Ok((result, false)),
            ChildSubmission::Resolved(result) => Ok((result, true)),
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
            replay_source_run_id,
            actor,
            action_key,
            retry_key,
            trusted_host,
            reconciliation,
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
        if !reconciliation && run_input_declares_review_reconciliation(&input) {
            return Err(reserved_reconciliation_key_error(job_name));
        }
        // [ORB-11333] The review admission follows the same discipline: a
        // child inherits its parent's snapshot, an ordinary delivery
        // submission captures the effective policy once, and ordinary input
        // naming the key is refused.
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
            let mut existing_automation_run = false;
            let mut seed_after_insert = false;
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
                    })? {
                    ChildJobRunAdmissionOutcome::Admitted(run) => *run,
                    ChildJobRunAdmissionOutcome::AdmissionsStopped => {
                        return Ok(ChildSubmission::Skipped("admissions_stopped".to_string()));
                    }
                }
            } else if let Some(key) = action_key {
                match self.stores().jobs().insert_automation_job_run(
                    job_name,
                    input.clone(),
                    key,
                )? {
                    KeyedJobRunAdmission::Admitted(run) => *run,
                    KeyedJobRunAdmission::Existing(run) => {
                        existing_automation_run = true;
                        *run
                    }
                }
            } else if let Some(retry_key) = retry_key {
                // [ORB-13560] The retry-key probe and the insert are one store
                // transaction, so concurrent submitters of one key — in any
                // process — admit one run. A resolved retry spawns nothing: the
                // original admission already delivered its worker.
                match self
                    .stores()
                    .jobs()
                    .insert_keyed_job_run(&KeyedJobRunParams {
                        job_id: job_name.to_string(),
                        retry_key_field: retry_key.field.to_string(),
                        scan_limit: retry_key.scan_limit,
                        scheduled_at: submitted_at,
                        input: input.clone(),
                    })? {
                    KeyedJobRunAdmission::Admitted(run) => {
                        seed_after_insert = true;
                        *run
                    }
                    KeyedJobRunAdmission::Existing(run) => {
                        let active_runs = self
                            .stores()
                            .jobs()
                            .list_pending_or_running_job_runs(job_name)?;
                        let queue_position = pipeline_run_queue_position(
                            &active_runs,
                            &run.run_id,
                            spec.max_active_runs,
                        );
                        return Ok(ChildSubmission::Resolved(PipelineInvokeResult {
                            run_id: run.run_id,
                            job_name: job_name.to_string(),
                            submitted_at: run.scheduled_at.to_rfc3339(),
                            queued: queue_position.is_some(),
                            queue_position,
                        }));
                    }
                }
            } else {
                // A resume admits through the lineage-guarded insert: while any
                // run in the source's retry lineage is live, it is refused with
                // `ResumeRunInFlight` in the same transaction that would have
                // inserted, so concurrent resumes from any surface yield one run.
                let run = match resume {
                    Some(plan) => self.stores().jobs().insert_resume_job_run(
                        job_name,
                        plan.attempt,
                        submitted_at,
                        Some(input.clone()),
                        &plan.source.run_id,
                    )?,
                    None => self.stores().jobs().insert_job_run(
                        job_name,
                        1,
                        submitted_at,
                        Some(input.clone()),
                        replay_source_run_id.map(ToOwned::to_owned),
                    )?,
                };
                seed_after_insert = true;
                run
            };

            let trigger = if admission.is_some() {
                JobRunTrigger::child()
            } else {
                trigger
            };
            // [ORB-14524] The run row is committed. A failure from here to the
            // worker handoff would otherwise strand a pending run with no
            // worker, holding its concurrency slot and retry/resume key until
            // the unclaimed-run grace expires, so it is terminalized before the
            // error is returned. A reused automation run belongs to its original
            // admission and keeps its state.
            let delivered = (|| -> Result<ChildSubmission, OrbitError> {
                if seed_after_insert {
                    self.seed_v2_pipeline_run(&run, &input, resume, trigger.clone())?;
                }
                // The transaction's outcome, rather than the run's
                // pending/running state, decides initialization. A retry may
                // resolve a pending run whose worker has already started
                // writing checkpoints or controls.
                if !existing_automation_run {
                    self.record_run_trigger(&run.run_id, &trigger)?;
                    // [ORB-14777] A child inherits its drain's environment, so
                    // only the submission that starts a process records it.
                    if admission.is_none() {
                        self.record_run_env_pass_unset(&run.run_id)?;
                    }
                }

                // Pin the definition before the worker can exist. A direct-path
                // submission must not depend on the source file surviving
                // unchanged until the detached worker gets around to reading it.
                if let SubmittedDefinition::Snapshot { yaml, .. } = &definition {
                    self.write_run_definition_snapshot(&run.run_id, yaml)?;
                }

                // Reaping other orphaned runs of the job does not concern this
                // submission; its failure must not fail an admitted run.
                log_best_effort(
                    "reconcile stale job runs",
                    &run.run_id,
                    self.reconcile_stale_job_runs(Some(job_name)),
                );
                let active_runs = self
                    .stores()
                    .jobs()
                    .list_pending_or_running_job_runs(job_name)?;
                let queue_position =
                    pipeline_run_queue_position(&active_runs, &run.run_id, spec.max_active_runs);
                let queued = queue_position.is_some();

                // A repeated automation admission resolves the original run.
                // Only pending runs need delivery; the existing Start CAS
                // fences workers.
                if (action_key.is_none() || run.state == JobRunState::Pending)
                    && let Err(error) = self.spawn_pipeline_worker(
                        &run.run_id,
                        actor,
                        input["__worker_containment_strict"] == true,
                    )
                {
                    let log_note =
                        match pipeline_worker_log_path(&self.paths().logs_dir, &run.run_id) {
                            Ok(worker_log) => format!("; worker log: '{}'", worker_log.display()),
                            Err(_) => String::new(),
                        };
                    let message = format!(
                        "pipeline worker for run '{}' could not start from registered \
                         workspace '{}': {error}{log_note}",
                        run.run_id,
                        self.paths().repo_root.display(),
                    );
                    let error_code =
                        matches!(error, OrbitError::WorkerContainmentUnavailable { .. })
                            .then_some(worker::scope::WORKER_CONTAINMENT_UNAVAILABLE_ERROR_CODE);
                    log_best_effort(
                        "finalize startup failure",
                        &run.run_id,
                        self.finalize_pipeline_worker_startup_failure(
                            &run, &message, error_code, actor,
                        ),
                    );
                    return Err(error);
                }
                Ok(ChildSubmission::Submitted(PipelineInvokeResult {
                    run_id: run.run_id.clone(),
                    job_name: job_name.to_string(),
                    submitted_at: submitted_at.to_rfc3339(),
                    queued,
                    queue_position,
                }))
            })();
            if let Err(error) = &delivered
                && !existing_automation_run
            {
                // A no-op when the spawn branch already terminalized the run.
                log_best_effort(
                    "finalize startup failure",
                    &run.run_id,
                    self.finalize_pipeline_worker_startup_failure(
                        &run,
                        &error.to_string(),
                        None,
                        actor,
                    ),
                );
            }
            delivered
        })();

        if let Some(plan) = resume {
            log_best_effort(
                "record resume submission audit",
                result
                    .as_ref()
                    .ok()
                    .and_then(ChildSubmission::run_id)
                    .unwrap_or_default(),
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
                ),
            );
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
    /// The YAML pinned beside `run_id`, when this run has one.
    ///
    /// Direct-path submission writes the snapshot so a later worker — and a
    /// later resume — executes that definition. A missing file means the run
    /// is catalog-backed. A symlink, non-regular file, or present file that
    /// cannot be read or parsed is an error: resume must not silently
    /// substitute a catalog asset of the same name.
    pub(crate) fn read_run_definition_snapshot(
        &self,
        run_id: &str,
    ) -> Result<Option<(JobV2, String)>, OrbitError> {
        let snapshot = run_definition_snapshot_path(&self.paths().job_runs_dir, run_id)?;
        // The shared filename validator confines the leaf to job_runs_dir.
        // Resolve its parent and open it without following the final component;
        // all checks and the read then use that same descriptor.
        let mut file = match open_read_only_no_follow(&snapshot) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(OrbitError::InvalidInput(format!(
                    "open job run definition snapshot '{}': {error}",
                    snapshot.display()
                )));
            }
        };
        let metadata = file.metadata().map_err(|error| {
            OrbitError::InvalidInput(format!("inspect {}: {error}", snapshot.display()))
        })?;
        if !metadata.is_file() {
            return Err(OrbitError::InvalidInput(format!(
                "job run definition snapshot must be a regular file: {}",
                snapshot.display()
            )));
        }
        let mut yaml = String::new();
        file.read_to_string(&mut yaml).map_err(|error| {
            OrbitError::InvalidInput(format!("read {}: {error}", snapshot.display()))
        })?;
        let asset = load_job_asset(&yaml).map_err(|error| {
            OrbitError::InvalidInput(format!("load {}: {error}", snapshot.display()))
        })?;
        Ok(Some((asset.spec, yaml)))
    }
    /// The definition a persisted run must execute: its own snapshot when the
    /// submission pinned one, otherwise the catalog asset named by the run.
    pub(crate) fn resolve_run_definition(
        &self,
        run: &JobRun,
    ) -> Result<(PathBuf, JobV2), OrbitError> {
        let snapshot = run_definition_snapshot_path(&self.paths().job_runs_dir, &run.run_id)?;
        if let Some((spec, _)) = self.read_run_definition_snapshot(&run.run_id)? {
            return Ok((snapshot, spec));
        }
        self.load_v2_job_asset_by_name(&run.job_id)
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
        self.stores()
            .jobs()
            .update_run_state(run_id, &mut |_, state| {
                state.trigger = Some(trigger.clone());
                Ok(())
            })?;
        Ok(())
    }

    /// [ORB-14777] Record which pass-listed variables this submitting process
    /// does not hold, so `run show` and the dashboard explain a worker that
    /// fell back to another login. Writes nothing when none are unset.
    fn record_run_env_pass_unset(&self, run_id: &str) -> Result<(), OrbitError> {
        let unset = self.unset_env_pass_names();
        if unset.is_empty() {
            return Ok(());
        }
        self.stores()
            .jobs()
            .update_run_state(run_id, &mut |_, state| {
                state.env_pass_unset = unset.clone();
                Ok(())
            })?;
        Ok(())
    }
}

pub(super) fn pipeline_run_is_runnable(
    runs: &[JobRun],
    run_id: &str,
    max_active_runs: u32,
) -> bool {
    runs.iter().any(|run| run.run_id == run_id)
        && pipeline_run_queue_position(runs, run_id, max_active_runs).is_none()
}

/// The run's one-based place among those waiting for the job's
/// `max_active_runs`, or `None` when it may execute now. A ceiling of `0` is
/// no ceiling: nothing ever waits.
fn pipeline_run_queue_position(
    runs: &[JobRun],
    run_id: &str,
    max_active_runs: u32,
) -> Option<usize> {
    if max_active_runs == 0 {
        return None;
    }
    let mut ordered = runs.to_vec();
    ordered.sort_by(|left, right| {
        left.scheduled_at
            .cmp(&right.scheduled_at)
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.run_id.cmp(&right.run_id))
    });
    ordered
        .iter()
        .position(|run| run.run_id == run_id)
        .and_then(|index| (index + 1).checked_sub(max_active_runs as usize))
        .filter(|position| *position > 0)
}

pub(crate) fn input_hash(input: &Value) -> String {
    let mut identity = input.clone();
    identity.sort_all_objects();
    let encoded = serde_json::to_vec(&identity).unwrap_or_default();
    sha256_hex(&encoded)
}
