use super::super::*;
use crate::application::contracts::RetryKey;

/// One durable pipeline submission: what to run, with what input, and how the
/// detached worker will find the definition again.
pub(crate) struct PipelineSubmission<'a> {
    pub(crate) job_name: &'a str,
    pub(crate) definition: SubmittedDefinition<'a>,
    pub(crate) input: Value,
    pub(crate) resume: Option<&'a ResumePlan>,
    /// Whole-run replay lineage, without inheriting resume checkpoints.
    pub(crate) replay_source_run_id: Option<&'a str>,
    pub(crate) actor: Option<&'a str>,
    pub(crate) action_key: Option<&'a str>,
    /// Caller retry key admitted atomically with the run [ORB-13560].
    pub(crate) retry_key: Option<RetryKey<'a>>,
    /// Whether this submission is the governed review reconciliation
    /// admission. Only it may carry
    /// [`REVIEW_RECONCILIATION_ADMISSION_KEY`](orbit_types::workflow::REVIEW_RECONCILIATION_ADMISSION_KEY).
    pub(crate) reconciliation: bool,
    /// How this run was submitted [ORB-12255].
    pub(crate) trigger: JobRunTrigger,
}

/// What a parent-authorized child submission produced.
#[derive(Debug, Clone)]
pub(crate) enum ChildSubmission {
    Submitted(PipelineInvokeResult),
    /// A keyed submission resolved the run an earlier submission of the same
    /// retry key admitted. Nothing was written and no worker was spawned.
    Resolved(PipelineInvokeResult),
    /// The atomic admission refused the child; `reason` is
    /// `admissions_stopped`.
    Skipped(String),
}

impl ChildSubmission {
    pub(in crate::application::job::pipeline) fn run_id(&self) -> Option<&str> {
        match self {
            ChildSubmission::Submitted(result) | ChildSubmission::Resolved(result) => {
                Some(result.run_id.as_str())
            }
            ChildSubmission::Skipped(_) => None,
        }
    }
}

impl<'a> PipelineSubmission<'a> {
    /// An ordinary submission: catalog definition, no resume, no idempotency
    /// key, and no trusted-host admission.
    pub(crate) fn catalog(job_name: &'a str, input: Value, actor: Option<&'a str>) -> Self {
        Self {
            job_name,
            definition: SubmittedDefinition::Catalog,
            input,
            resume: None,
            replay_source_run_id: None,
            actor,
            action_key: None,
            retry_key: None,
            reconciliation: false,
            trigger: JobRunTrigger::cli(),
        }
    }

    fn with_trigger(mut self, trigger: JobRunTrigger) -> Self {
        self.trigger = trigger;
        self
    }
}

/// Trusted context for a pipeline child submitted by a running v2 activity.
///
/// The parent run id comes from the engine-owned [`orbit_tools::ToolContext`],
/// never from tool input. The remaining fields make the parent link complete
/// at the same atomic boundary that creates the child.
#[derive(Debug, Clone)]
pub(crate) struct ChildPipelineAdmission {
    pub parent_run_id: String,
    pub parent_step_id: Option<String>,
    pub action: String,
    pub blocking: bool,
}

/// How a submitted run's definition reaches its worker.
pub(crate) enum SubmittedDefinition<'a> {
    /// Resolve `job_name` from the catalog, at submission and again in the
    /// worker. Catalog assets are managed, so name resolution stays the
    /// contract for them.
    Catalog,
    /// Pin this exact validated YAML alongside the run [ORB-10801]. A direct
    /// path is an unmanaged file the submitter happened to name; rereading it
    /// from a detached worker would let an edit or deletion between submission
    /// and execution change (or destroy) the run.
    Snapshot { spec: &'a JobV2, yaml: &'a str },
}

impl OrbitRuntime {
    /// Submit a run for a catalog job id or a direct schemaVersion 2 job YAML
    /// path — the shared entry point behind `orbit run job` / `orbit job run`.
    ///
    /// [ORB-10801] Submission is one-shot: it validates the definition,
    /// persists the run, and hands it to a detached worker. A direct path is
    /// snapshotted next to the run before this returns, so the worker executes
    /// exactly the definition that was validated even if the source file is
    /// edited or deleted a moment later.
    pub fn submit_job_run(
        &self,
        job_ref: &str,
        input: Value,
        actor: Option<&str>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        let direct_path = Path::new(job_ref);
        if !direct_path.is_file() {
            return self.submit_catalog_job_run(job_ref, input, actor, JobRunTrigger::cli());
        }

        let (job_name, spec, yaml) = self.load_direct_job_definition(direct_path)?;
        let result = self.submit_persisted_pipeline_run(PipelineSubmission {
            definition: SubmittedDefinition::Snapshot {
                spec: &spec,
                yaml: &yaml,
            },
            ..PipelineSubmission::catalog(&job_name, input.clone(), actor)
        });
        self.record_submission_audit(&job_name, &input, actor, &result);
        result
    }

    /// Submit an operator UI's no-input catalog action through the shared dispatcher.
    pub fn submit_no_input_catalog_job_run(
        &self,
        job_id: &str,
        actor: Option<&str>,
        trigger: JobRunTrigger,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        let entry = self.show_job_catalog_entry(job_id)?;
        if entry.kind() != orbit_types::workflow::JobKind::Subroutine
            && !entry.supports_no_input_submission()
        {
            return Err(OrbitError::InvalidInput(format!(
                "job '{job_id}' requires task input or a delivery window, or is disabled/subroutine; use Ship or Drain for delivery"
            )));
        }
        self.submit_catalog_job_run(job_id, json!({}), actor, trigger)
    }

    /// Submit a catalog job by id. This entry point never interprets the id as
    /// a path, so request surfaces can reject direct files while preserving the
    /// same catalog validation and subroutine refusal as the CLI. `trigger` is
    /// the submitting surface's provenance [ORB-13016].
    pub fn submit_catalog_job_run(
        &self,
        job_id: &str,
        input: Value,
        actor: Option<&str>,
        trigger: JobRunTrigger,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        let entry = self.show_job_catalog_entry(job_id)?;
        if entry.kind() == orbit_types::workflow::JobKind::Subroutine {
            return Err(OrbitError::InvalidInput(format!(
                "job '{}' declares `kind: subroutine` and cannot be run directly (asset: {}).",
                entry.job_id,
                entry.path.display()
            )));
        }
        self.submit_pipeline_run_with_trigger(&entry.job_id, input, None, actor, trigger)
    }

    /// Read and fully validate a direct-path job definition in the submitting
    /// process, so a broken asset is refused before any run is persisted.
    fn load_direct_job_definition(
        &self,
        yaml_path: &Path,
    ) -> Result<(String, JobV2, String), OrbitError> {
        let yaml = std::fs::read_to_string(yaml_path).map_err(|error| {
            OrbitError::InvalidInput(format!("read {}: {error}", yaml_path.display()))
        })?;
        let asset = load_job_asset(&yaml).map_err(|error| {
            OrbitError::InvalidInput(format!("load {}: {error}", yaml_path.display()))
        })?;
        Ok((asset.name, asset.spec, yaml))
    }

    /// Submit an automation-admitted run under its idempotency `key`.
    /// `trigger` names what admitted it [ORB-13016].
    pub(crate) fn submit_automation_pipeline_run(
        &self,
        job_name: &str,
        input: Value,
        key: &str,
        trigger: JobRunTrigger,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        let result = self.submit_persisted_pipeline_run(
            PipelineSubmission {
                action_key: Some(key),
                ..PipelineSubmission::catalog(job_name, input.clone(), Some("automation"))
            }
            .with_trigger(trigger),
        );
        self.record_submission_audit(job_name, &input, Some("automation"), &result);
        result
    }

    pub fn submit_pipeline_run(
        &self,
        job_name: &str,
        input: Value,
        priority: Option<&str>,
        actor: Option<&str>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        self.submit_pipeline_run_with_trigger(
            job_name,
            input,
            priority,
            actor,
            JobRunTrigger::cli(),
        )
    }

    pub(crate) fn submit_pipeline_run_with_trigger(
        &self,
        job_name: &str,
        input: Value,
        priority: Option<&str>,
        actor: Option<&str>,
        trigger: JobRunTrigger,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        let result = self.submit_persisted_pipeline_run(
            PipelineSubmission::catalog(job_name, input.clone(), actor).with_trigger(trigger),
        );

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
                match &result {
                    Ok(_) => AuditEventStatus::Success,
                    Err(_) => AuditEventStatus::Failure,
                },
                json!({
                    "actor": actor,
                    "job_name": job_name,
                    "priority": priority,
                    "run_id": result.as_ref().ok().map(|value| value.run_id.clone()),
                    "input_hash": input_hash(&input),
                }),
                result.as_ref().err().map(|error| error.to_string()),
            ),
        );

        result
    }

    /// Submit a v2 activity's child through the parent's durable admission
    /// boundary [ORB-11310].
    ///
    /// `Ok(None)` is the benign, idempotent result when the parent auto drain
    /// has already acknowledged an admissions stop. Direct/non-child callers
    /// continue to use [`Self::submit_pipeline_run`] and are unchanged.
    pub(crate) fn submit_child_pipeline_run(
        &self,
        job_name: &str,
        input: Value,
        priority: Option<&str>,
        actor: Option<&str>,
        admission: &ChildPipelineAdmission,
    ) -> Result<ChildSubmission, OrbitError> {
        let result = self.submit_persisted_pipeline_run_with_admission(
            PipelineSubmission::catalog(job_name, input.clone(), actor)
                .with_trigger(JobRunTrigger::child()),
            Some(admission),
        );

        log_best_effort(
            "record child submission audit",
            result
                .as_ref()
                .ok()
                .and_then(ChildSubmission::run_id)
                .unwrap_or_default(),
            self.record_pipeline_audit(
                "pipeline.invoke",
                result.as_ref().ok().and_then(ChildSubmission::run_id),
                actor,
                match &result {
                    Ok(_) => AuditEventStatus::Success,
                    Err(_) => AuditEventStatus::Failure,
                },
                json!({
                    "actor": actor,
                    "job_name": job_name,
                    "priority": priority,
                    "parent_run_id": admission.parent_run_id,
                    "outcome": match &result {
                        Ok(ChildSubmission::Skipped(reason)) => reason.as_str(),
                        _ => "submitted",
                    },
                    "run_id": result.as_ref().ok().and_then(ChildSubmission::run_id),
                    "input_hash": input_hash(&input),
                }),
                result.as_ref().err().map(|error| error.to_string()),
            ),
        );

        result
    }
}
