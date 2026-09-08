//! `orbit job run <yaml-path>` — schemaVersion 2 job entrypoint.
//!
//! Mirrors `activity_v2::run_activity_v2_from_yaml`: reads the YAML, routes
//! through the two-pass loader, and dispatches via the Phase 3 DAG executor.
//! orbit-core never names orbit-agent types — transport/session construction
//! lives below the boundary in `orbit_engine::job_executor`.

use std::path::Path;

use orbit_common::{NotFoundKind, OrbitError};
use orbit_engine::activity_job::load_job_asset;
use orbit_engine::{
    JobOutcome, V2AuditWriter, dispatch_error_to_orbit, execute_job_with_resume,
    resolve_job_catalog_refs_for_execution,
};
use orbit_store::contracts::{JobRunStepParams, TaskReservationReleaseReason};
use orbit_types::record::OrbitEvent;
use orbit_types::workflow::activity_job::{
    V2AuditEventKind, run_input_declares_trusted_host, strip_trusted_host_admission,
    validate_job_retired_sessions,
};
use orbit_types::workflow::{
    JobRun, JobRunStartOutcome, JobRunState, JobTargetType, PipelineState,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::SYSTEM_AUDIT_IDENTITY;
use crate::application::job::resume::ResumePlan;

#[derive(Debug, Clone)]
pub struct V2JobRunResult {
    pub run_id: String,
    pub job_name: String,
    pub success: bool,
    pub pipeline: Value,
    pub message: Option<String>,
    pub events_emitted: u64,
}

/// Path-specific durable records retained while finalizing a v2 run.
///
/// The foreground path predates per-step checkpointing, so its legacy summary
/// row and synthetic success step remain observable API. Detached workers
/// already persist their authoritative per-step checkpoints and must not add a
/// synthetic step 0 that can obscure them. Keeping that distinction explicit
/// lets both paths share the terminal-state, reservation, and event sequence.
#[derive(Debug, Clone, Copy)]
pub(crate) struct V2RunFinalizationOptions {
    write_legacy_summary_step: bool,
    record_synthetic_success_step: bool,
}

impl V2RunFinalizationOptions {
    pub(crate) const DIRECT: Self = Self {
        write_legacy_summary_step: true,
        record_synthetic_success_step: true,
    };

    pub(crate) const DETACHED_WORKER: Self = Self {
        write_legacy_summary_step: false,
        record_synthetic_success_step: false,
    };
}

impl OrbitRuntime {
    /// Execute a v2 Job from a YAML file. Returns a structural result. The
    /// file must declare `schemaVersion: 2` and `kind: Job`; v1 files are
    /// rejected.
    pub fn run_job_v2_from_yaml(
        &self,
        yaml_path: &Path,
        input: Value,
    ) -> Result<V2JobRunResult, OrbitError> {
        self.run_job_v2_from_yaml_with_retry_source(yaml_path, input, None, 1, None)
    }

    /// Re-run a completed or historical job run from step 0 using the current
    /// catalog definition and the source run's persisted input.
    ///
    /// Explicitly discards checkpoints — replay is the "run everything again"
    /// surface, including agent steps. Use `resume_job_run` / `submit_resume_run`
    /// to continue from the failed step instead.
    pub fn replay_job_run(&self, source_run_id: &str) -> Result<V2JobRunResult, OrbitError> {
        let source = self.show_job_run(source_run_id)?;
        let mut input = source.input.clone().unwrap_or_else(|| json!({}));
        // [ORB-11354] A replay re-runs a historical input under no new
        // admission, so the source's trusted-host admission does not travel
        // with it. Stripping rather than refusing keeps replay usable for the
        // rest of the run's input; the activity then fails closed on the
        // missing admission, which is the honest outcome.
        strip_trusted_host_admission(&mut input);
        let (job_path, _) = self.load_v2_job_asset_by_name(&source.job_id)?;
        self.run_job_v2_from_yaml_with_retry_source(
            &job_path,
            input,
            Some(source.run_id.clone()),
            1,
            None,
        )
    }

    /// [ORB-10002] Resume an interrupted (or failed / timed-out) job run from
    /// its persisted step checkpoints.
    ///
    /// Creates a new run linked via `retry_source_run_id`, seeds its
    /// `PipelineState` from the source run's checkpoints, and executes the
    /// job skipping every top-level step already recorded as `success` —
    /// their outputs are fed back into the pipeline so later steps see them.
    /// If the source run has no successful checkpoints this degrades to a
    /// full replay.
    ///
    /// [ORB-10470] Before the first step runs, the resume reconciles the tasks
    /// its own retry lineage owns: a task blocked by the source failure is
    /// re-admitted, and its `job_run_id` is realigned to the batch id the
    /// reused checkpoints carry. Tasks owned by an unrelated run are untouched.
    ///
    /// This surface runs the job **in-process** and returns only at a terminal
    /// state — it is the foreground CLI path (`orbit job resume`). Non-blocking
    /// callers (the HTTP API, bridge) use
    /// [`OrbitRuntime::submit_resume_run`](crate::OrbitRuntime::submit_resume_run).
    pub fn resume_job_run(&self, source_run_id: &str) -> Result<V2JobRunResult, OrbitError> {
        let plan = self.plan_job_run_resume(source_run_id)?;
        self.run_job_v2_from_yaml_with_retry_source(
            &plan.job_path,
            plan.input.clone(),
            Some(plan.source.run_id.clone()),
            plan.attempt,
            Some(&plan),
        )
    }

    fn run_job_v2_from_yaml_with_retry_source(
        &self,
        yaml_path: &Path,
        input: Value,
        retry_source_run_id: Option<String>,
        attempt: u32,
        resume: Option<&ResumePlan>,
    ) -> Result<V2JobRunResult, OrbitError> {
        let job_name = load_job_name(yaml_path)?;
        // [ORB-11354] The foreground path takes caller-shaped input too, so it
        // gets the same reserved-key refusal as the detached submission path.
        // Nothing runs a trusted-host activity in the foreground: an admitted
        // invocation is always a detached run so it survives disconnect.
        if run_input_declares_trusted_host(&input) {
            return Err(super::pipeline::reserved_trusted_host_key_error(&job_name));
        }
        let scheduled_at = chrono::Utc::now();
        let run = self.stores().jobs().insert_job_run(
            &job_name,
            attempt,
            scheduled_at,
            Some(input.clone()),
            retry_source_run_id.clone(),
        )?;
        self.seed_v2_pipeline_run(&run, &input, resume)?;

        let started_at = chrono::Utc::now();
        // [ORB-10965] This run was inserted moments ago by this very process,
        // so it must win its own Start. Anything else means another owner
        // reached it first, and running it here would duplicate execution.
        match self.stores().jobs().mark_job_run_running(
            &run.run_id,
            started_at,
            std::process::id(),
        )? {
            JobRunStartOutcome::Started => {}
            JobRunStartOutcome::NotFound => {
                return Err(OrbitError::not_found(NotFoundKind::JobRun, run.run_id));
            }
            JobRunStartOutcome::AlreadyStarted => {
                return Err(OrbitError::JobRunStartConflict(format!(
                    "run '{}' was already started by this process; \
                     refusing to execute it a second time",
                    run.run_id
                )));
            }
        }
        self.record_run_crew_from_input(&run.run_id, &input)?;
        self.record_event(OrbitEvent::JobRunStarted {
            job_id: run.job_id.clone(),
            run_id: run.run_id.clone(),
            attempt: run.attempt,
        })?;

        let outcome = self.run_job_v2_from_yaml_with_run_context(
            yaml_path,
            input.clone(),
            Some(run.run_id.clone()),
            retry_source_run_id,
            resume.and_then(|plan| plan.resume_state.as_ref()),
        );
        let finished_at = chrono::Utc::now();

        self.finalize_v2_pipeline_run(
            &run,
            &input,
            started_at,
            finished_at,
            outcome.as_ref(),
            V2RunFinalizationOptions::DIRECT,
        )?;
        outcome
    }

    pub fn run_job_v2_from_yaml_with_run_id(
        &self,
        yaml_path: &Path,
        input: Value,
        run_id_override: Option<String>,
    ) -> Result<V2JobRunResult, OrbitError> {
        self.run_job_v2_from_yaml_with_run_context(yaml_path, input, run_id_override, None, None)
    }

    /// [ORB-10470] Execute a persisted run against its own checkpoints.
    ///
    /// The pipeline worker uses this so a run whose `PipelineState` already
    /// records successful top-level steps — a resumed run seeded at submission,
    /// or a run whose earlier worker died after checkpointing — continues from
    /// the first non-successful step instead of replaying completed work.
    pub(crate) fn run_job_v2_from_yaml_with_run_id_and_resume(
        &self,
        yaml_path: &Path,
        input: Value,
        run_id_override: Option<String>,
        retry_source_run_id: Option<String>,
        resume: Option<&PipelineState>,
    ) -> Result<V2JobRunResult, OrbitError> {
        self.run_job_v2_from_yaml_with_run_context(
            yaml_path,
            input,
            run_id_override,
            retry_source_run_id,
            resume,
        )
    }

    fn run_job_v2_from_yaml_with_run_context(
        &self,
        yaml_path: &Path,
        input: Value,
        run_id_override: Option<String>,
        retry_source_run_id: Option<String>,
        resume: Option<&PipelineState>,
    ) -> Result<V2JobRunResult, OrbitError> {
        let yaml = std::fs::read_to_string(yaml_path).map_err(|err| {
            OrbitError::InvalidInput(format!("read {}: {err}", yaml_path.display()))
        })?;
        let mut asset = load_job_asset(&yaml).map_err(|err| {
            OrbitError::InvalidInput(format!("load {}: {err}", yaml_path.display()))
        })?;

        // Phase 4: resolve `target: activity:<name>` refs before any other
        // pass, so retired-feature rejection sees concrete specs.
        let catalog = self
            .v2_activity_catalog()
            .map_err(|err| OrbitError::InvalidInput(format!("build activity catalog: {err}")))?;
        resolve_job_catalog_refs_for_execution(&mut asset.spec, &catalog)
            .map_err(dispatch_error_to_orbit)?;

        // [ORB-10801] Reject retired declarations at load time so a run never
        // starts a DAG it cannot finish as written.
        validate_job_retired_sessions(&asset.spec, &yaml_path.display().to_string())
            .map_err(|err| OrbitError::InvalidInput(format!("{err}")))?;
        let run_id = run_id_override.unwrap_or_else(|| {
            format!(
                "job-{}-{}",
                asset.name,
                chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f")
            )
        });

        let audit_root = self.paths().audit_dir.clone();
        let workspace_path = self.paths().repo_root.clone();
        let writer = V2AuditWriter::with_disk_sinks(
            &audit_root,
            self.v2_audit_store()?,
            self.workspace_id()?,
            &run_id,
            SYSTEM_AUDIT_IDENTITY,
            Some(workspace_path.as_path()),
        )
        .map_err(|err| OrbitError::Execution(format!("audit sinks: {err}")))?;
        self.record_event(OrbitEvent::ActivityRunStarted {
            id: asset.name.clone(),
        })?;
        let _ = writer.emit(V2AuditEventKind::RunStarted {
            job_name: format!("cli:{}", asset.name),
            retry_source_run_id,
        });

        let outcome_res: Result<JobOutcome, OrbitError> =
            execute_job_with_resume(&asset.spec, input, &run_id, writer.clone(), self, resume)
                .map_err(|err| OrbitError::Execution(format!("v2 job dispatch: {err}")));

        let (outcome_str, error_message) = match &outcome_res {
            Ok(o) if o.success => ("success", None),
            Ok(o) => ("failed", o.message.clone()),
            Err(err) => ("error", Some(err.to_string())),
        };
        let _ = writer.emit(V2AuditEventKind::RunFinished {
            outcome: outcome_str.to_string(),
            error_message,
        });
        self.record_event(OrbitEvent::ActivityRunCompleted {
            id: asset.name.clone(),
            state: outcome_str.to_string(),
        })?;

        let events_count = writer.emitted_event_count();

        match outcome_res {
            Ok(o) => Ok(V2JobRunResult {
                run_id,
                job_name: asset.name,
                success: o.success,
                pipeline: o.pipeline,
                message: o.message,
                events_emitted: events_count,
            }),
            Err(err) => Err(err),
        }
    }

    /// Seed a persisted run before either the foreground or detached path
    /// executes it. A resume re-keys checkpoints and reconciles the failed
    /// lineage's task ownership before admission evaluates the first step.
    pub(crate) fn seed_v2_pipeline_run(
        &self,
        run: &JobRun,
        input: &Value,
        resume: Option<&ResumePlan>,
    ) -> Result<(), OrbitError> {
        let mut initial_state = match resume.and_then(|plan| plan.resume_state.as_ref()) {
            Some(source_state) => seeded_resume_state(source_state, run),
            None => PipelineState::new(run.run_id.clone(), run.job_id.clone(), input.clone()),
        };
        // [ORB-11283] A queued drain can carry an operator stop (or worker
        // ceiling) written before the worker seeded this document. Replacing
        // the whole state would silently resume admissions.
        if let Some(existing) = self.read_run_state(&run.run_id)? {
            if initial_state.drain_worker_limit.is_none() {
                initial_state.drain_worker_limit = existing.drain_worker_limit;
            }
            if initial_state.drain_admissions_stop.is_none() {
                initial_state.drain_admissions_stop = existing.drain_admissions_stop;
            }
            if initial_state.child_dispatches.is_empty() {
                initial_state.child_dispatches = existing.child_dispatches;
            }
        }
        self.stores()
            .jobs()
            .write_run_state(&run.run_id, &initial_state)?;
        if let Some(plan) = resume {
            self.reconcile_resume_task_ownership(plan, &run.run_id)?;
        }
        Ok(())
    }

    /// Persist the common terminal lifecycle for foreground and detached v2
    /// execution without changing their established checkpoint conventions.
    pub(crate) fn finalize_v2_pipeline_run(
        &self,
        run: &JobRun,
        input: &Value,
        started_at: chrono::DateTime<chrono::Utc>,
        finished_at: chrono::DateTime<chrono::Utc>,
        outcome: Result<&V2JobRunResult, &OrbitError>,
        options: V2RunFinalizationOptions,
    ) -> Result<(), OrbitError> {
        let duration_ms = Some(
            finished_at
                .signed_duration_since(started_at)
                .num_milliseconds()
                .max(0) as u64,
        );
        let final_state = match outcome {
            Ok(result) if result.success => {
                self.persist_v2_run_state(run, input, result, JobRunState::Success, options)?;
                if options.record_synthetic_success_step {
                    self.record_synthetic_v2_success_step(run, started_at, finished_at, result)?;
                }
                JobRunState::Success
            }
            Ok(result) => {
                self.persist_v2_run_state(run, input, result, JobRunState::Failed, options)?;
                let fallback = "job completed with success=false but emitted no failure detail";
                let message = result.message.as_deref().unwrap_or(fallback);
                let _ = self.record_pipeline_failure_step(run, started_at, finished_at, message);
                JobRunState::Failed
            }
            Err(error) => {
                let _ = self.record_pipeline_failure_step(
                    run,
                    started_at,
                    finished_at,
                    &error.to_string(),
                );
                JobRunState::Failed
            }
        };
        self.finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            final_state,
            finished_at,
            duration_ms,
            TaskReservationReleaseReason::RunTerminal,
        )?;
        self.record_event(OrbitEvent::JobRunCompleted {
            job_id: run.job_id.clone(),
            run_id: run.run_id.clone(),
            state: final_state.to_string(),
        })
    }

    fn persist_v2_run_state(
        &self,
        run: &JobRun,
        input: &Value,
        result: &V2JobRunResult,
        final_state: JobRunState,
        options: V2RunFinalizationOptions,
    ) -> Result<(), OrbitError> {
        let mut state = self.read_run_state(&run.run_id)?.unwrap_or_else(|| {
            PipelineState::new(run.run_id.clone(), run.job_id.clone(), input.clone())
        });
        state.sync_pipeline(result.pipeline.clone());
        // [ORB-10002] Per-step checkpoints already maintain step records for
        // this run; only fall back to the legacy single-summary step record
        // when no checkpoint was ever written, so a later `resume` never sees
        // a whole-run summary clobbering step 0's real checkpoint.
        if options.write_legacy_summary_step && state.step_states.is_empty() {
            state.record_step(0, final_state, Some(result.pipeline.clone()), None);
        }
        self.stores().jobs().write_run_state(&run.run_id, &state)
    }

    fn record_synthetic_v2_success_step(
        &self,
        run: &JobRun,
        started_at: chrono::DateTime<chrono::Utc>,
        finished_at: chrono::DateTime<chrono::Utc>,
        result: &V2JobRunResult,
    ) -> Result<(), OrbitError> {
        let duration_ms = Some(
            finished_at
                .signed_duration_since(started_at)
                .num_milliseconds()
                .max(0) as u64,
        );
        self.stores().jobs().complete_job_run_step(
            &run.run_id,
            &JobRunStepParams {
                step_index: 0,
                target_type: JobTargetType::Job,
                target_id: run.job_id.clone(),
                started_at,
                finished_at,
                duration_ms,
                exit_code: Some(0),
                agent_response_json: Some(result.pipeline.clone()),
                state: JobRunState::Success,
                error_code: None,
                error_message: None,
            },
        )?;
        Ok(())
    }
}

/// [ORB-10002] Re-key a source run's checkpoint state onto the resumed run.
/// The step records are the source's; only the identity and timestamp change,
/// so the resumed run owns its own durable state from its first write.
pub(super) fn seeded_resume_state(source_state: &PipelineState, run: &JobRun) -> PipelineState {
    let mut seeded = source_state.clone();
    seeded.run_id = run.run_id.clone();
    seeded.job_id = run.job_id.clone();
    seeded.updated_at = chrono::Utc::now();
    seeded
}

fn load_job_name(yaml_path: &Path) -> Result<String, OrbitError> {
    let yaml = std::fs::read_to_string(yaml_path)
        .map_err(|err| OrbitError::InvalidInput(format!("read {}: {err}", yaml_path.display())))?;
    let asset = load_job_asset(&yaml)
        .map_err(|err| OrbitError::InvalidInput(format!("load {}: {err}", yaml_path.display())))?;
    Ok(asset.name)
}
