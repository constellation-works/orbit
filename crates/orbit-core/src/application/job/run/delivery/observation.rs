use super::*;

pub(super) const COMMIT_ACTIVITY: &str = "git_commit";
pub(super) const PR_LANDING_ACTIVITY: &str = "pr_complete";
pub(super) const LOCAL_LANDING_ACTIVITY: &str = "git_merge";

/// Longest run or task id this read accepts.
pub(super) const MAX_ID_LEN: usize = 128;
/// Only parent links are read; no sibling or broad-history search.
pub(super) const MAX_ANCESTOR_HOPS: usize = 64;
/// Longest PR number accepted from a checkpoint.
pub(super) const MAX_PR_NUMBER_DIGITS: usize = 12;

/// One top-level host step the observation reads, located in the job
/// definition rather than guessed from a step index.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeliveryStep {
    pub(crate) id: String,
    pub(crate) index: u32,
    pub(crate) activity: &'static str,
    definition: JobV2Step,
}

/// The delivery-relevant steps of one job definition.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct DeliveryJobShape {
    pub(crate) commit: Option<DeliveryStep>,
    pub(crate) landings: Vec<DeliveryStep>,
}

impl DeliveryJobShape {
    /// Only resolved catalog targets whose spec is the deterministic host
    /// action count: an agent activity that happens to be named like one is
    /// not host evidence.
    pub(crate) fn from_job(job: &JobV2) -> Self {
        let mut shape = Self::default();
        for (index, step) in job.steps.iter().enumerate() {
            let Ok(index) = u32::try_from(index) else {
                break;
            };
            let Some(activity) = host_activity(step) else {
                continue;
            };
            let step = DeliveryStep {
                id: step.id.clone(),
                index,
                activity,
                definition: step.clone(),
            };
            if activity == COMMIT_ACTIVITY {
                if shape.commit.is_none() {
                    shape.commit = Some(step);
                }
            } else {
                shape.landings.push(step);
            }
        }
        shape
    }
}

pub(super) fn host_activity(step: &JobV2Step) -> Option<&'static str> {
    let JobV2StepBody::Target(TargetStep {
        spec: ActivityV2Spec::Deterministic(spec),
        activity_name: Some(name),
        ..
    }) = &step.body
    else {
        return None;
    };
    [COMMIT_ACTIVITY, PR_LANDING_ACTIVITY, LOCAL_LANDING_ACTIVITY]
        .into_iter()
        .find(|activity| name == activity && spec.action == *activity)
}

/// The catalogs one observation resolves definitions from, each parsed at
/// most once however many runs it reads.
#[derive(Default)]
pub(super) struct DeliveryDefinitions {
    jobs: V2JobNameCache,
    activities: OnceCell<Option<V2ActivityCatalog>>,
}

/// Run and job ids by submitted task, newest run first.
pub(super) type RunsByTask = HashMap<String, Vec<(String, String)>>;

/// What a codebase scan reuses across the delivery observations it makes:
/// the repository identity, this workspace's runs indexed by submitted task,
/// and the definitions those runs resolve. Each is read once, on the first
/// observation that needs it.
#[derive(Default)]
pub(crate) struct CodebaseDeliveryScan {
    repository: OnceCell<Option<String>>,
    runs: OnceCell<Result<RunsByTask, String>>,
    definitions: DeliveryDefinitions,
}

impl OrbitRuntime {
    /// Observe what one task-delivery run committed and landed for one task
    /// of this workspace. Read-only: the run is read as stored, without the
    /// stale-run reconciliation an operator read performs.
    pub fn observe_run_delivery(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<RunDeliveryObservation, OrbitError> {
        let run_id = bounded_id(run_id, "run_id")?;
        let task_id = bounded_id(task_id, "task_id")?;
        let run = self.show_job_run_observed(run_id)?;
        let task = self.get_task(task_id)?;
        if !run_task_ids(&run).contains(&task.id.as_str()) {
            return Err(OrbitError::InvalidInput(format!(
                "run '{run_id}' did not deliver task '{task_id}' in this workspace"
            )));
        }
        self.project_stored_delivery(
            &run,
            &task.id,
            || self.delivery_repository(),
            &DeliveryDefinitions::default(),
        )
    }

    /// [ORB-14661] Observe what a claimed leaf run committed and landed for
    /// its claimed task, from this executor's own record of the leaf. The
    /// claimed task lives in its owner's store, never this one, so ownership
    /// is the durable admission binding the leaf to the claim on that task,
    /// already authorized by the caller, in place of the task-store lookup
    /// [`Self::observe_run_delivery`] makes. The owner holds no record of the
    /// leaf, so only this side can answer for it.
    pub(crate) fn observe_claimed_leaf_delivery(
        &self,
        leaf: &crate::application::job::claimed::ClaimedLeaf,
    ) -> Result<RunDeliveryObservation, OrbitError> {
        let run_id = leaf.binding.bound_run_id.as_str();
        let task_id = leaf.claim.task_id.as_str();
        let run = self.show_job_run_observed(run_id)?;
        if !run_task_ids(&run).contains(&task_id) {
            return Err(OrbitError::InvalidInput(format!(
                "run '{run_id}' did not deliver its claimed task"
            )));
        }
        self.project_stored_delivery(
            &run,
            task_id,
            || self.delivery_repository(),
            &DeliveryDefinitions::default(),
        )
    }

    /// The observation of `run` for `task_id`, whose ownership the caller
    /// has already established.
    pub(super) fn project_stored_delivery(
        &self,
        run: &JobRun,
        task_id: &str,
        repository: impl FnOnce() -> Option<String>,
        definitions: &DeliveryDefinitions,
    ) -> Result<RunDeliveryObservation, OrbitError> {
        let run_id = run.run_id.as_str();
        let shape = self.delivery_shape(run, definitions)?;
        let state = self.read_run_state(run_id)?;
        let repository = repository();
        let executed = shape
            .as_ref()
            .map(|shape| self.delivery_execution_records(run, shape));
        let commit = match &executed {
            Some(Err(gap)) => {
                commit_gap(*gap, shape.as_ref().and_then(|shape| shape.commit.as_ref()))
            }
            _ => self.observe_stored_commit(
                executed
                    .as_ref()
                    .and_then(|run| run.as_ref().ok())
                    .unwrap_or(run),
                state.as_ref(),
                shape.as_ref(),
                task_id,
                definitions,
            ),
        };
        let landing = observe_landing(
            executed
                .as_ref()
                .and_then(|run| run.as_ref().ok())
                .unwrap_or(run),
            state.as_ref(),
            shape.as_ref(),
            &commit,
        );
        let delivery_status = summarize(run.state, &commit, &landing);
        Ok(RunDeliveryObservation {
            schema_version: RUN_DELIVERY_SCHEMA_VERSION,
            workspace_id: self.workspace_id()?,
            repository,
            task_id: task_id.to_string(),
            run_id: run.run_id.clone(),
            job_id: run.job_id.clone(),
            run_state: run.state,
            run_finished_at: run.finished_at,
            delivery_status,
            commit,
            landing,
        })
    }

    /// Resolve catalog refs exactly as execution does, using the pinned
    /// definition where one exists. An unresolved or non-host action never
    /// authenticates a checkpoint.
    pub(super) fn delivery_shape(
        &self,
        run: &JobRun,
        definitions: &DeliveryDefinitions,
    ) -> Result<Option<DeliveryJobShape>, OrbitError> {
        let Some(mut job) = self.run_definition(&run.run_id, &run.job_id, definitions) else {
            return Ok(None);
        };
        if !job.holds_task_delivery() {
            return Err(OrbitError::InvalidInput(format!(
                "run '{}' is not a task delivery run",
                run.run_id
            )));
        }
        let activities = definitions
            .activities
            .get_or_init(|| self.v2_activity_catalog().ok());
        Ok(activities.as_ref().and_then(|catalog| {
            orbit_engine::resolve_job_catalog_refs_for_execution(&mut job, catalog).ok()?;
            Some(DeliveryJobShape::from_job(&job))
        }))
    }

    /// The definition [`Self::resolve_run_definition`] resolves for a run:
    /// its pinned snapshot, else the catalog job it names. `None` when
    /// neither can be read.
    pub(super) fn run_definition(
        &self,
        run_id: &str,
        job_id: &str,
        definitions: &DeliveryDefinitions,
    ) -> Option<JobV2> {
        match self.read_run_definition_snapshot(run_id) {
            Ok(Some((job, _))) => Some(job),
            Ok(None) => self.v2_job_by_name_cached(&definitions.jobs, job_id),
            Err(_) => None,
        }
    }

    pub(super) fn observe_stored_commit(
        &self,
        run: &JobRun,
        state: Option<&PipelineState>,
        shape: Option<&DeliveryJobShape>,
        task_id: &str,
        definitions: &DeliveryDefinitions,
    ) -> CommitObservation {
        let ordinary = observe_commit(run, state, shape, task_id);
        let (Some(state), Some(step)) = (state, shape.and_then(|shape| shape.commit.as_ref()))
        else {
            return ordinary;
        };
        if !state_belongs_to(state, run) {
            return commit_gap(DeliveryEvidenceGap::OwnershipMismatch, Some(step));
        }
        let Checkpoint::Recorded(output) = checkpoint(state, step) else {
            return ordinary;
        };
        let owner = output.get("job_run_id").and_then(Value::as_str);
        // Older checkpoints omit the owner. When a retry has no executed
        // commit record, authenticate its producer rather than attributing the
        // copied bytes (and a tail-local record's timestamp) to the retry.
        let inherited = owner.is_some_and(|id| id != run.run_id.as_str())
            || (run.retry_source_run_id.is_some() && !executed_step(run, step));
        if !inherited {
            return ordinary;
        }
        match self.inherited_commit(run, state, step, output, task_id, definitions) {
            Ok(commit) => commit,
            Err(gap) => commit_gap(gap, Some(step)),
        }
    }

    /// Authenticate every parent link all the way to the root, even after
    /// finding the producer, so a cycle or truncated lineage cannot hide
    /// behind a matching checkpoint. Only the copied prefix through the
    /// producer must contain identical checkpoint bytes.
    pub(super) fn inherited_commit(
        &self,
        run: &JobRun,
        current_state: &PipelineState,
        step: &DeliveryStep,
        output: &Value,
        task_id: &str,
        definitions: &DeliveryDefinitions,
    ) -> Result<CommitObservation, DeliveryEvidenceGap> {
        if !inherited_checkpoint_matches(current_state, step, output) {
            return Err(DeliveryEvidenceGap::CheckpointInconsistent);
        }
        let mut seen = HashSet::from([run.run_id.clone()]);
        let mut cursor = run.retry_source_run_id.clone();
        let mut producer = None;
        let mut hops = 0;
        while let Some(parent_id) = cursor {
            if hops >= MAX_ANCESTOR_HOPS
                || bounded_id(&parent_id, "run_id").is_err()
                || !seen.insert(parent_id.clone())
            {
                return Err(DeliveryEvidenceGap::OwnershipMismatch);
            }
            hops += 1;
            // This query is scoped by the answering runtime's workspace.
            let parent = self
                .show_job_run_observed(&parent_id)
                .map_err(|_| DeliveryEvidenceGap::OwnershipMismatch)?;
            if parent.job_id != run.job_id || !run_task_ids(&parent).contains(&task_id) {
                return Err(DeliveryEvidenceGap::OwnershipMismatch);
            }
            let shape = self
                .delivery_shape(&parent, definitions)
                .ok()
                .flatten()
                .ok_or(DeliveryEvidenceGap::JobDefinitionUnavailable)?;
            if shape.commit.as_ref() != Some(step) {
                return Err(DeliveryEvidenceGap::CheckpointInconsistent);
            }
            let parent = self.delivery_execution_records(&parent, &shape)?;
            let state = self
                .read_run_state(&parent_id)
                .ok()
                .flatten()
                .ok_or(DeliveryEvidenceGap::StateMissing)?;
            if !state_belongs_to(&state, &parent) {
                return Err(DeliveryEvidenceGap::OwnershipMismatch);
            }
            if producer.is_none() {
                if !inherited_checkpoint_matches(&state, step, output) {
                    return Err(DeliveryEvidenceGap::CheckpointInconsistent);
                }
                if executed_step(&parent, step) {
                    // The producer's recognized host contract checks both task
                    // and run ownership. An ancestor merely named in input is
                    // insufficient, even if the retry claims success.
                    parse_commit_output(output, &parent.run_id, task_id)?;
                    producer = Some(observe_commit(&parent, Some(&state), Some(&shape), task_id));
                }
            }
            cursor = parent.retry_source_run_id;
        }
        producer.ok_or(DeliveryEvidenceGap::OwnershipMismatch)
    }
}

impl OrbitRuntime {
    /// Observe what a delivery run committed and landed for one task: the
    /// named run, or, when none is named, the newest task-delivery run this
    /// workspace recorded with the task among its submitted `task_ids`.
    ///
    /// The run is chosen from the same evidence the observation checks — the
    /// run's own submitted input and a job that holds task delivery — so the
    /// default never picks a run the explicit form would refuse.
    pub fn observe_task_delivery(
        &self,
        task_id: &str,
        run_id: Option<&str>,
    ) -> Result<RunDeliveryObservation, OrbitError> {
        if let Some(run_id) = run_id {
            return self.observe_run_delivery(run_id, task_id);
        }
        let task_id = bounded_id(task_id, "task_id")?;
        let task = self.get_task(task_id)?;
        // Filter submitted task bindings before hydrating run inputs, newest
        // first. No global cap: newer non-delivery jobs must not hide the
        // latest delivery run. Steps are not needed to choose the run.
        let runs = self.list_job_runs_filtered_backend(&JobRunQuery {
            task_id: Some(task.id.clone()),
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        let run_id = self.latest_delivery_run_id(
            &task.id,
            runs.iter()
                .filter(|run| run_task_ids(run).contains(&task.id.as_str()))
                .map(|run| (run.run_id.as_str(), run.job_id.as_str())),
            &DeliveryDefinitions::default(),
        )?;
        self.observe_run_delivery(&run_id, &task.id)
    }

    /// [`Self::observe_task_delivery`] for one task of a codebase scan, which
    /// read `task_id` from this workspace's own task listing: that listing
    /// establishes the task's ownership, so the task is not read again. The
    /// run is still read, and must name the task, before any evidence is.
    /// Everything `scan` holds is resolved once across the scan, including
    /// a failed resolution.
    pub(crate) fn observe_codebase_task_delivery(
        &self,
        task_id: &str,
        scan: &CodebaseDeliveryScan,
    ) -> Result<RunDeliveryObservation, OrbitError> {
        tracing::debug!(task_id, codebase_delivery_observation = true);
        let task_id = bounded_id(task_id, "task_id")?;
        let runs = scan
            .runs
            .get_or_init(|| {
                self.delivery_runs_by_task()
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|error| OrbitError::Store(error.clone()))?;
        let run_id = self.latest_delivery_run_id(
            task_id,
            runs.get(task_id)
                .into_iter()
                .flatten()
                .map(|(run_id, job_id)| (run_id.as_str(), job_id.as_str())),
            &scan.definitions,
        )?;
        let run = self.show_job_run_observed(&run_id)?;
        if !run_task_ids(&run).contains(&task_id) {
            return Err(OrbitError::InvalidInput(format!(
                "run '{run_id}' did not deliver task '{task_id}' in this workspace"
            )));
        }
        self.project_stored_delivery(
            &run,
            task_id,
            || {
                scan.repository
                    .get_or_init(|| self.delivery_repository())
                    .clone()
            },
            &scan.definitions,
        )
    }

    /// Every run of this workspace by the tasks it was submitted with, newest
    /// first, from one listing without steps.
    pub(super) fn delivery_runs_by_task(&self) -> Result<RunsByTask, OrbitError> {
        let mut by_task = RunsByTask::new();
        let runs = self.list_job_runs_filtered_backend(&JobRunQuery {
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        for run in &runs {
            for task_id in run_task_ids(run).into_iter().collect::<HashSet<_>>() {
                by_task
                    .entry(task_id.to_string())
                    .or_default()
                    .push((run.run_id.clone(), run.job_id.clone()));
            }
        }
        Ok(by_task)
    }

    pub(super) fn delivery_repository(&self) -> Option<String> {
        crate::application::automation::source::Source::new(&self.paths().repo_root)
            .repository()
            .ok()
    }

    /// The first of `runs`, the task's runs newest first as `(run_id,
    /// job_id)`, whose definition holds task delivery. A direct-path run
    /// judges its own pinned snapshot; catalog-backed runs of one job share
    /// the catalog's verdict.
    pub(super) fn latest_delivery_run_id<'a>(
        &self,
        task_id: &str,
        runs: impl Iterator<Item = (&'a str, &'a str)>,
        definitions: &DeliveryDefinitions,
    ) -> Result<String, OrbitError> {
        for (run_id, job_id) in runs {
            if self
                .run_definition(run_id, job_id, definitions)
                .is_some_and(|job| job.holds_task_delivery())
            {
                return Ok(run_id.to_string());
            }
        }
        Err(OrbitError::InvalidInput(format!(
            "no task delivery run in this workspace was submitted with task '{task_id}'"
        )))
    }
}
