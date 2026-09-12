use super::*;

/// [ORB-10544] Cap on the run history scanned by the in-flight ship guard.
/// Non-terminal runs are always among the newest rows, so a bounded window is
/// enough to spot a duplicate dispatch without walking the whole history.
const SHIP_IN_FLIGHT_SCAN_LIMIT: usize = 200;

/// One durable pipeline submission: what to run, with what input, and how the
/// detached worker will find the definition again.
pub(crate) struct PipelineSubmission<'a> {
    pub(crate) job_name: &'a str,
    pub(crate) definition: SubmittedDefinition<'a>,
    pub(crate) input: Value,
    pub(crate) resume: Option<&'a ResumePlan>,
    pub(crate) actor: Option<&'a str>,
    pub(crate) action_key: Option<&'a str>,
    /// Whether this submission is the canonical trusted-host admission
    /// [ORB-11354]. Only it may carry [`TRUSTED_HOST_ADMISSION_KEY`] in its
    /// input; every other submission is refused for supplying it.
    pub(crate) trusted_host: bool,
    /// Whether this submission is the grant-bound drain coordinator
    /// [ORB-11332]. Only it (and the parent-authorized child path, which
    /// copies the parent's snapshot) may carry [`OPERATION_ADMISSION_KEY`].
    pub(crate) operation_bound: bool,
    /// How this run was submitted [ORB-12255].
    pub(crate) trigger: JobRunTrigger,
}

/// What a parent-authorized child submission produced.
#[derive(Debug, Clone)]
pub(crate) enum ChildSubmission {
    Submitted(PipelineInvokeResult),
    /// The atomic admission refused the child; `reason` is
    /// `admissions_stopped` or one of the grant-bound refusals.
    Skipped(String),
}

impl ChildSubmission {
    pub(super) fn run_id(&self) -> Option<&str> {
        match self {
            ChildSubmission::Submitted(result) => Some(result.run_id.as_str()),
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
            actor,
            action_key: None,
            trusted_host: false,
            operation_bound: false,
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
    /// Submit a `ship` workflow run (the `task_auto_pipeline` job).
    ///
    /// Shared entry point for every non-interactive submission surface
    /// (dashboard HTTP endpoint, MCP `orbit.workflow.ship`, `orbit run
    /// ship-sweep`). `base_branch` falls back to the workspace's `[workflow]
    /// base_branch`; an empty `task_ids` slice selects auto
    /// (backlog-discovery) mode. One-shot: returns as soon as the run is
    /// persisted and its worker spawned.
    ///
    /// [ORB-10544] An explicit task selection is guarded against duplicate
    /// dispatch here rather than in any one adapter: if a named task is already
    /// carried by a non-terminal run, the submission is refused with
    /// [`OrbitError::ShipRunInFlight`] naming that task and run, so two runs
    /// cannot contend for one worktree and task reservation no matter which
    /// surface submitted them. Auto mode has no task ids to key on and is
    /// unaffected.
    ///
    /// [ORB-11187] `completion` is the caller's explicit authorization for this
    /// run to finish delivery and perform the guarded `review -> done`
    /// transition. It defaults to
    /// [`CompletionPolicy::Review`](crate::application::workflow::CompletionPolicy::Review)
    /// at every surface and is only ever raised by a per-invocation operator
    /// flag; nothing derives it from workspace configuration or the environment.
    ///
    /// [ORB-10709] The workspace claim is checked first, for the case the
    /// duplicate-dispatch guard structurally cannot cover: it is keyed on task
    /// id over a bounded window of recent runs, so a stale non-terminal run
    /// outside that window is invisible to it, and a discovery-mode submission
    /// carries no task ids at all. The claim check is keyed on neither, so both
    /// gaps close. `claim_token` is the holder's minted token; `None` falls back
    /// to [`CLAIM_TOKEN_ENV`](crate::runtime::workspace_claim::CLAIM_TOKEN_ENV).
    // Existing public positional API; keep callers stable while submission is composed internally.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_ship_run(
        &self,
        mode: crate::application::workflow::ShipMode,
        base_branch: Option<&str>,
        task_ids: &[String],
        completion: crate::application::workflow::CompletionPolicy,
        allowed_crews: &[String],
        actor: Option<&str>,
        claim_token: Option<&str>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        self.require_workspace_claim("orbit.workflow.ship", claim_token)?;
        let workflow = crate::application::workflow::find_workflow(
            crate::application::workflow::SHIP_WORKFLOW_ALIAS,
        )
        .ok_or_else(|| OrbitError::InvalidInput("unknown workflow 'ship'".to_string()))?;
        let base = base_branch.unwrap_or_else(|| self.workflow_base_branch());
        let allowed_crews = self.canonical_allowed_crews(allowed_crews)?;
        let allowlist = self.crew_allowlist(&allowed_crews)?;
        let input = crate::application::workflow::build_ship_input(
            mode,
            base,
            task_ids,
            completion,
            &allowed_crews,
        )?;
        // Validate explicit selections before inspecting runs or creating a
        // pipeline record. Auto mode intentionally carries no task ids: the
        // worker discovers eligible backlog tasks after it starts.
        for task_id in task_ids {
            let task = self.get_task(task_id)?;
            if task.tags.iter().any(|tag| tag == "epic") {
                return Err(OrbitError::InvalidInput(format!(
                    "task '{task_id}' is an epic root and cannot be shipped as a leaf; use `orbit run auto` or `orbit run job epic_pipeline`"
                )));
            }
            if let Some(allowlist) = allowlist.as_ref() {
                let crew = self.effective_task_crew(&task)?;
                crate::runtime::engine::crew::enforce_crew_allowlist(
                    Some(allowlist),
                    &crew,
                    &format!("explicit ship task '{task_id}'"),
                )?;
            }
        }
        if let Some(conflict) = self.in_flight_ship_run_for_tasks(task_ids)? {
            return Err(conflict);
        }
        self.submit_pipeline_run(workflow.job_id, input, None, actor)
    }
    /// Submit one workspace drain (`workspace_auto_pipeline`).
    ///
    /// [ORB-10819] `for_seconds` is the drain window: the run keeps re-listing
    /// admissible work and shipping it until the window expires. `None` (or
    /// zero) means one tick, which is what every caller predating the window
    /// gets. The window bounds only the *start* of new work — a child run
    /// already in flight when the deadline passes finishes normally.
    /// `max_active_leaf_runs` is the drain's concurrency ceiling: how many
    /// `task_auto_pipeline` children may be live at once. Omitted, the job's
    /// own default applies — this only forwards an explicit override, so the
    /// default lives in one place, next to the loop that reads it.
    ///
    /// [ORB-11242] `allowed_crews` is the run-scoped crew restriction. Empty
    /// means unrestricted, which is what every caller predating it gets. Names
    /// are resolved against this host's `[crews.*]` registry *here*, before a
    /// run record exists, so an unknown or blank name fails the submission
    /// rather than quietly shrinking what a live drain admits; the canonical
    /// registry names are what gets persisted and forwarded. It gates what the
    /// drain may *start* — it does not touch workspace configuration, reassign
    /// a task's crew, or cancel work another invocation already has in flight.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_workspace_auto_run(
        &self,
        for_seconds: Option<u64>,
        max_active_leaf_runs: Option<u32>,
        completion: crate::application::workflow::CompletionPolicy,
        allowed_crews: &[String],
        complexity_crews: &orbit_config::ComplexityCrewPools,
        actor: Option<&str>,
        claim_token: Option<&str>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        self.require_workspace_claim("orbit.workflow.auto", claim_token)?;
        let workflow = crate::application::workflow::find_workflow(
            crate::application::workflow::AUTO_WORKFLOW_ALIAS,
        )
        .ok_or_else(|| OrbitError::InvalidInput("unknown workflow 'auto'".to_string()))?;
        let mut input = workspace_auto_run_input(
            for_seconds,
            max_active_leaf_runs,
            completion,
            &self.canonical_allowed_crews(allowed_crews)?,
        )?;
        Self::set_auto_crew_overrides(&mut input, complexity_crews);
        self.submit_pipeline_run(workflow.job_id, input, None, actor)
    }
    /// Canonicalize an operator-supplied crew allowlist, rejecting blank or
    /// unconfigured names [ORB-11242].
    ///
    /// Canonical registry names are persisted rather than the operator's
    /// spelling, so the durable run input says exactly which configured crews
    /// the window permits regardless of the alias that was typed.
    pub(crate) fn canonical_allowed_crews(
        &self,
        allowed_crews: &[String],
    ) -> Result<Vec<String>, OrbitError> {
        let mut canonical: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for name in allowed_crews {
            if name.trim().is_empty() {
                return Err(OrbitError::InvalidInput(
                    "crew name in the allowlist must not be empty".to_string(),
                ));
            }
            let Some(resolved) = self.canonical_crew_name(Some(name))? else {
                return Err(OrbitError::InvalidInput(
                    "crew name in the allowlist must not be empty".to_string(),
                ));
            };
            canonical.insert(resolved);
        }
        Ok(canonical.into_iter().collect())
    }
    /// The duplicate-dispatch refusal for the newest non-terminal run already
    /// carrying one of `task_ids`, or `None` when the selection is free.
    ///
    /// A run's task selection lives in its persisted `input.task_ids`, which is
    /// what [`Self::submit_ship_run`] writes, so this sees every prior
    /// submission regardless of the surface that made it.
    fn in_flight_ship_run_for_tasks(
        &self,
        task_ids: &[String],
    ) -> Result<Option<OrbitError>, OrbitError> {
        if task_ids.is_empty() {
            return Ok(None);
        }
        let runs = self.list_job_runs(crate::application::job::JobRunListParams {
            limit: Some(SHIP_IN_FLIGHT_SCAN_LIMIT),
            ..Default::default()
        })?;
        Ok(runs.into_iter().find_map(|run| {
            if run.state.is_terminal() {
                return None;
            }
            let task_id = run
                .input
                .as_ref()
                .and_then(|input| input.get("task_ids"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .find(|candidate| task_ids.iter().any(|wanted| wanted == candidate))
                .map(str::to_string)?;
            Some(OrbitError::ShipRunInFlight {
                task_id,
                run_id: run.run_id,
            })
        }))
    }
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
            let entry = self.show_job_catalog_entry(job_ref)?;
            if entry.kind() == orbit_types::workflow::JobKind::Subroutine {
                return Err(OrbitError::InvalidInput(format!(
                    "job '{}' declares `kind: subroutine` and cannot be run directly (asset: {}).",
                    entry.job_id,
                    entry.path.display()
                )));
            }
            return self.submit_pipeline_run(&entry.job_id, input, None, actor);
        }

        let (job_name, spec, yaml) = self.load_direct_job_definition(direct_path)?;
        let result = self.submit_persisted_pipeline_run(PipelineSubmission {
            definition: SubmittedDefinition::Snapshot {
                spec: &spec,
                yaml: &yaml,
            },
            ..PipelineSubmission::catalog(&job_name, input.clone(), actor)
        });
        self.record_submission_audit(&job_name, &input, actor, &result)?;
        result
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
        validate_job_retired_sessions(&asset.spec, &yaml_path.display().to_string())
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        Ok((asset.name, asset.spec, yaml))
    }
    pub(crate) fn submit_automation_pipeline_run(
        &self,
        job_name: &str,
        input: Value,
        key: &str,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        let result = self.submit_persisted_pipeline_run(PipelineSubmission {
            action_key: Some(key),
            ..PipelineSubmission::catalog(job_name, input.clone(), Some("automation"))
        });
        self.record_submission_audit(job_name, &input, Some("automation"), &result)?;
        result
    }
    /// The only submission permitted to write [`OPERATION_ADMISSION_KEY`]:
    /// the grant-bound drain coordinator [ORB-11332]. Children inherit the
    /// snapshot at the parent-authorized admission path, never from input.
    pub(crate) fn submit_operation_bound_pipeline_run(
        &self,
        job_name: &str,
        input: Value,
        actor: Option<&str>,
    ) -> Result<PipelineInvokeResult, OrbitError> {
        let result = self.submit_persisted_pipeline_run(PipelineSubmission {
            operation_bound: true,
            ..PipelineSubmission::catalog(job_name, input.clone(), actor)
        });
        self.record_submission_audit(job_name, &input, actor, &result)?;
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
        )?;

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
        )?;

        result
    }
}
