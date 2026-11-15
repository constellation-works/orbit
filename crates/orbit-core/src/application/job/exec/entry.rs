use super::V2RunFinalizationOptions;
use crate::OrbitRuntime;
use crate::application::SYSTEM_AUDIT_IDENTITY;
use crate::application::job::pipeline::{PipelineInvokeResult, PipelineSubmission};
use crate::application::job::resume::ResumePlan;
use crate::application::job::{
    JobAutoConcurrencyPort, JobAutomationPort, JobDrainPort, ReviewAdmissionHook,
};
use crate::application::job::{crew_pools, delivery, log_best_effort, pipeline, resume};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_engine::activity_job::load_job_asset;
use orbit_engine::{
    JobOutcome, V2AuditWriter, dispatch_error_to_orbit, execute_job_with_resume,
    resolve_job_catalog_refs_with_scripts_for_execution,
};
use orbit_types::record::OrbitEvent;
use orbit_types::workflow::activity_job::{
    V2AuditEventKind, run_input_declares_trusted_host, strip_trusted_host_admission,
};
use orbit_types::workflow::{JobRun, JobRunStartOutcome, JobRunTrigger, PipelineState};
use serde_json::{Value, json};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct V2JobRunResult {
    pub run_id: String,
    pub job_name: String,
    pub success: bool,
    /// Named external evidence that stopped delivery without a failure.
    pub evidence_hold: Option<orbit_types::workflow::ReviewEvidenceHold>,
    /// A delivery push the forge kept refusing; the run holds for a resume.
    pub forge_hold: Option<orbit_types::workflow::ForgeUnavailableHold>,
    /// A child the run waited on was cancelled and none failed: the run ends
    /// `cancelled`, not `failed` [ORB-15202].
    pub child_cancelled: bool,
    pub pipeline: Value,
    pub message: Option<String>,
    /// The typed step-failure code of the step the run failed on, when it
    /// declared one.
    pub failure_code: Option<orbit_types::workflow::StepFailureCode>,
    pub events_emitted: u64,
    /// [ORB-20158] The run parked on an `auto-task:` step: it stays live,
    /// holds no worker, and the owner clock resumes it. Never finalized.
    pub parked: Option<orbit_types::workflow::ParkedWait>,
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
        self.run_job_v2_from_yaml_with_retry_source(yaml_path, input, None, 1, None, None)
    }

    /// Re-run a completed or historical job run from step 0 using the current
    /// catalog definition and the source run's persisted input.
    ///
    /// Automatic crews are readmitted under current pools and provider holds;
    /// a recorded explicit crew choice is retained.
    /// Explicitly discards checkpoints — replay is the "run everything again"
    /// surface, including agent steps. Use `submit_resume_run`
    /// to continue from the failed step instead.
    pub fn replay_job_run(&self, source_run_id: &str) -> Result<V2JobRunResult, OrbitError> {
        self.replay_job_run_with_claim(source_run_id, None)
    }

    /// Foreground replay with the operator's workspace claim token.
    pub fn replay_job_run_with_claim(
        &self,
        source_run_id: &str,
        claim_token: Option<&str>,
    ) -> Result<V2JobRunResult, OrbitError> {
        let (source, mut input) = self.admit_job_run_replay(source_run_id, claim_token)?;
        self.install_review_admission(&source.job_id, &mut input, None, false)?;
        let task_writes = self.install_auto_crew_admission(
            &source.job_id,
            &mut input,
            None,
            false,
            &mut crew_pools::random_crew_ticket,
        )?;
        let (job_path, _) = self.load_v2_job_asset_by_name(&source.job_id)?;
        self.run_job_v2_from_yaml_with_retry_source(
            &job_path,
            input,
            Some(source.run_id),
            1,
            None,
            task_writes,
        )
    }

    /// Persist a whole-run replay and submit it to a detached worker.
    /// Uses today's catalog and admission policy, with no source checkpoints.
    pub fn submit_replay_run(
        &self,
        source_run_id: &str,
        actor: Option<&str>,
        claim_token: Option<&str>,
        trigger: JobRunTrigger,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        let (source, input) = self.admit_job_run_replay(source_run_id, claim_token)?;
        self.submit_persisted_pipeline_run(PipelineSubmission {
            replay_source_run_id: Some(&source.run_id),
            trigger,
            ..PipelineSubmission::catalog(&source.job_id, input, actor)
        })
    }

    fn admit_job_run_replay(
        &self,
        source_run_id: &str,
        claim_token: Option<&str>,
    ) -> Result<(JobRun, Value), OrbitError> {
        self.require_workspace_claim("orbit.job.replay", claim_token)?;
        let source = self.show_job_run(source_run_id)?;
        let mut input = source.input.clone().unwrap_or_else(|| json!({}));
        let task_ids: Vec<String> = resume::task_ids_from_input(&input)
            .unwrap_or_default()
            .into_iter()
            .collect();
        self.admit_explicit_drain_entry(
            crate::application::contracts::DrainEntryPoint::Replay,
            &task_ids,
        )?;
        // Historical authority never travels into a new invocation. Trusted
        // host execution fails closed; review is captured from current policy.
        strip_trusted_host_admission(&mut input);
        orbit_types::workflow::strip_review_reconciliation_admission(&mut input);
        if let Some(object) = input.as_object_mut() {
            object.remove(orbit_types::workflow::REVIEW_ADMISSION_KEY);
        }
        crew_pools::strip_auto_crew_admission(&source.job_id, &mut input);
        Ok((source, input))
    }

    fn run_job_v2_from_yaml_with_retry_source(
        &self,
        yaml_path: &Path,
        input: Value,
        retry_source_run_id: Option<String>,
        attempt: u32,
        resume: Option<&ResumePlan>,
        admission_task_writes: Option<crew_pools::AdmissionTaskWrites>,
    ) -> Result<V2JobRunResult, OrbitError> {
        let (job_name, spec) = load_job_definition(yaml_path)?;
        // An auto-task step parks its run and resumes it from the owner
        // clock, which only a detached run admits (ORB-20158).
        if !spec.auto_task_steps().is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "job '{job_name}' has auto-task steps, which run only as a detached \
                 submission; nothing was run"
            )));
        }
        // [ORB-11354] The foreground path takes caller-shaped input too, so it
        // gets the same reserved-key refusal as the detached submission path.
        // Nothing runs a trusted-host activity in the foreground: an admitted
        // invocation is always a detached run so it survives disconnect.
        if input
            .get(orbit_types::workflow::SOURCE_REPAIR_KEY)
            .is_some()
        {
            return Err(OrbitError::CapabilityDenied(
                "source repair input is reserved for explicit operator admission".into(),
            ));
        }
        if run_input_declares_trusted_host(&input) {
            return Err(pipeline::reserved_trusted_host_key_error(&job_name));
        }
        if orbit_types::workflow::run_input_declares_review_reconciliation(&input) {
            return Err(pipeline::reserved_reconciliation_key_error(&job_name));
        }
        let mut input = input;
        delivery::normalize_delivery_input(&spec, &mut input)?;
        self.install_auto_concurrency(
            &job_name,
            &mut input,
            resume.is_some() || retry_source_run_id.is_some(),
        )?;
        let scheduled_at = chrono::Utc::now();
        let run = self.stores().jobs().insert_job_run(
            &job_name,
            attempt,
            scheduled_at,
            Some(input.clone()),
            retry_source_run_id.clone(),
        )?;
        // [ORB-19485] The run is durably inserted, so the draw's task writes
        // may land. A failure before this point wrote nothing to the task.
        if let Some(writes) = admission_task_writes {
            self.apply_admission_task_writes(writes);
        }
        // [ORB-14524] The run row is committed, so a seed or Start-write
        // failure must terminalize it: no worker exists to ever pick it up, and
        // a stranded pending run would hold its concurrency slot and retry key.
        let started_at = chrono::Utc::now();
        let start = self
            .seed_v2_pipeline_run(&run, &input, resume, JobRunTrigger::cli())
            .and_then(|()| {
                self.stores().jobs().mark_job_run_running(
                    &run.run_id,
                    started_at,
                    std::process::id(),
                )
            })
            .inspect_err(|error| {
                log_best_effort(
                    "finalize startup failure",
                    &run.run_id,
                    self.finalize_pipeline_worker_startup_failure(
                        &run,
                        &error.to_string(),
                        None,
                        None,
                    ),
                );
            })?;
        // [ORB-10965] This run was inserted moments ago by this very process,
        // so it must win its own Start. Anything else means another owner
        // reached it first, and running it here would duplicate execution.
        match start {
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
        // Once Start succeeds, setup errors belong to this run and must reach
        // the same terminal lifecycle as execution errors in detached workers.
        let outcome = (|| {
            let bundle = self.run_bundle_crews(&run, &input);
            self.record_run_crew_for_job(&run.run_id, &input, yaml_path, bundle)?;
            self.record_event(OrbitEvent::JobRunStarted {
                job_id: run.job_id.clone(),
                run_id: run.run_id.clone(),
                attempt: run.attempt,
            })?;

            self.run_job_v2_from_yaml_with_run_context(
                yaml_path,
                input.clone(),
                Some(run.run_id.clone()),
                retry_source_run_id,
                resume.and_then(|plan| plan.resume_state.as_ref()),
            )
        })();
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
        // pass. The workspace's script catalog rides along so a `target:
        // script:<name>` step loads (its manifest is validated and its
        // entry resolved inside the workspace) and runs at dispatch once
        // its grant checks out; a missing or invalid manifest fails this
        // load, naming the manifest file.
        let catalog = self
            .v2_activity_catalog()
            .map_err(|err| OrbitError::InvalidInput(format!("build activity catalog: {err}")))?;
        let scripts = self.v2_script_catalog()?;
        resolve_job_catalog_refs_with_scripts_for_execution(&mut asset.spec, &catalog, &scripts)
            .map_err(dispatch_error_to_orbit)?;

        let run_id = run_id_override.unwrap_or_else(|| {
            format!(
                "job-{}-{}",
                asset.name,
                chrono::Utc::now().format("%Y%m%dT%H%M%S%.3f")
            )
        });

        // Preserve the exact executing definition for consumers of checkpointed
        // script evidence, including foreground runs without submission snapshots.
        self.write_run_definition_snapshot(&run_id, &yaml)?;

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
        let audit_job_name = self
            .read_run_state(&run_id)
            .ok()
            .flatten()
            .and_then(|state| state.trigger)
            .unwrap_or_else(JobRunTrigger::cli)
            .audit_job_name(&asset.name);
        writer.emit_lossy(V2AuditEventKind::RunStarted {
            job_name: audit_job_name,
            retry_source_run_id,
        });

        let outcome_res: Result<JobOutcome, OrbitError> =
            execute_job_with_resume(&asset.spec, input, &run_id, writer.clone(), self, resume)
                .map_err(|err| match err {
                    orbit_engine::DispatchError::ProtocolSkew(message) => {
                        OrbitError::ProtocolSkew(message)
                    }
                    refusal @ orbit_engine::DispatchError::DeliveryRefused { .. } => {
                        dispatch_error_to_orbit(refusal)
                    }
                    // Keep the typed footprint code so finalization holds
                    // the run instead of failing it.
                    hold @ orbit_engine::DispatchError::FootprintUnavailable(_) => {
                        dispatch_error_to_orbit(hold)
                    }
                    other => OrbitError::Execution(format!("v2 job dispatch: {other}")),
                });

        let (outcome_str, error_message) = super::audit_terminal_outcome(&asset.name, &outcome_res);
        // A parked run has not finished: the worker that resumes it records
        // the run's end.
        if outcome_str != "parked" {
            writer.emit_lossy(V2AuditEventKind::RunFinished {
                outcome: outcome_str.to_string(),
                error_message,
            });
            self.record_event(OrbitEvent::ActivityRunCompleted {
                id: asset.name.clone(),
                state: outcome_str.to_string(),
            })?;
        }

        let events_count = writer.emitted_event_count();

        match outcome_res {
            Ok(o) => Ok(V2JobRunResult {
                run_id,
                job_name: asset.name,
                success: o.success,
                evidence_hold: o.evidence_hold,
                forge_hold: o.forge_hold,
                child_cancelled: o.child_cancelled,
                pipeline: o.pipeline,
                message: o.message,
                failure_code: o.failure_code,
                events_emitted: events_count,
                parked: o.parked,
            }),
            Err(err) => Err(err),
        }
    }
}

fn load_job_definition(
    yaml_path: &Path,
) -> Result<(String, orbit_types::workflow::JobV2), OrbitError> {
    let yaml = std::fs::read_to_string(yaml_path)
        .map_err(|err| OrbitError::InvalidInput(format!("read {}: {err}", yaml_path.display())))?;
    let asset = load_job_asset(&yaml)
        .map_err(|err| OrbitError::InvalidInput(format!("load {}: {err}", yaml_path.display())))?;
    Ok((asset.name, asset.spec))
}
