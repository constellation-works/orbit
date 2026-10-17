//! Publication survives a final-recovery requeue independently of failure handoff.
//!
//! Two publications are preserved this way: the run's own pipeline push and
//! PR, which a final-recovery `requeue` keeps, and the `[BLOCKED]` PR a
//! failure handoff published, which an operator or cleared-infra requeue of
//! the blocked task keeps (`failure_handoff`). Both are re-authenticated in
//! full from the immutable source when the next run adopts them.
use super::*;
use serde::{Deserialize, Serialize};

const PRESERVED: &str = "published_owner_requeued";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PreservedPublication {
    schema_version: u32,
    source_run_id: String,
    task_digest: String,
    /// The publication is the source's failure-handoff PR, authenticated
    /// from its handoff receipt rather than pipeline checkpoints.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    failure_handoff: bool,
}

fn preserved_entry(record: &PreservedPublication) -> Result<TaskHistoryEntry, OrbitError> {
    Ok(TaskHistoryEntry {
        at: Utc::now(),
        by: "system".into(),
        event: PRESERVED.into(),
        note: Some(serde_json::to_string(record).map_err(|e| refuse(e.to_string()))?),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    })
}

impl OrbitRuntime {
    /// Called under the final-recovery task lock; stored atomically with its decision.
    pub(crate) fn published_owner_requeue_record(
        &self,
        task: &Task,
        run_id: &str,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        let Some(source) = self.get_job_run_backend(run_id)? else {
            return Ok(None);
        };
        let Some(state) = self.read_run_state(run_id)? else {
            return Ok(None);
        };
        if source.job_id != "task_pr_pipeline"
            || source.input.as_ref().is_some_and(|i| i["claimed"] == true)
            || !["push", "pr_open"]
                .iter()
                .any(|key| state.pipeline.get(key).is_some_and(|v| !v.is_null()))
        {
            return Ok(None);
        }
        let record = PreservedPublication {
            schema_version: 1,
            source_run_id: run_id.into(),
            task_digest: task_digest(task)?,
            failure_handoff: false,
        };
        preserved_entry(&record).map(Some)
    }

    /// Authenticate the `[BLOCKED]` PR the failure handoff of `task`'s
    /// current run published, exactly as the next run's adoption will, and
    /// return the preservation record that lets that run resume it, with the
    /// binding it resumes. Called by a requeue of the blocked task before it
    /// takes the task write lock, since authenticating asks the remote and
    /// the forge; nothing is written here.
    pub(crate) fn failure_handoff_requeue_record(
        &self,
        task: &Task,
    ) -> Result<(TaskHistoryEntry, SourceRepairBinding), OrbitError> {
        let source_id = task
            .job_run_id
            .as_deref()
            .ok_or_else(|| refuse("the task has no run whose failure handoff it could resume"))?;
        let source = self
            .get_job_run_backend(source_id)?
            .ok_or_else(|| refuse("the task's run is missing"))?;
        self.owner_source_identity(&source, task)?;
        let entries = self.get_task_history(&task.id)?;
        if source.state != JobRunState::Failed
            || source.finished_at.is_none()
            || run_owner_liveness(&source) != RunOwnerLiveness::Stopped
        {
            return Err(refuse(
                "the task's run is not a conclusively stopped failed delivery",
            ));
        }
        if entries.iter().any(|e| {
            e.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT && e.at >= source.created_at
        }) {
            return Err(refuse("an operator discarded this run's candidate"));
        }
        // The same newest-execution rule the adopting run applies: nothing
        // newer may stand between the task and this publication.
        let newest = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some("task_pr_pipeline".into()),
            task_id: Some(task.id.clone()),
            limit: Some(1),
            include_steps: false,
            ..Default::default()
        })?;
        if newest.first().is_none_or(|run| run.run_id != source_id) {
            return Err(refuse(
                "newer execution makes publication association ambiguous",
            ));
        }
        let state = self
            .read_run_state(source_id)?
            .ok_or_else(|| refuse("source checkpoints missing"))?;
        if state.failure_activity_checkpoint.is_none() {
            return Err(refuse("the run published no failure-handoff PR"));
        }
        let definition = self.source_repair_definition(&source)?;
        let (_, _, binding) =
            self.observe_handoff_publication(source, task.clone(), state, definition)?;
        let record = PreservedPublication {
            schema_version: 1,
            source_run_id: source_id.into(),
            task_digest: task_digest(task)?,
            failure_handoff: true,
        };
        Ok((preserved_entry(&record)?, binding))
    }

    /// Whether `task`, last delivered by an owner PR run, has a pull request
    /// or a publication-preservation record on record. With no
    /// failure-handoff receipt binding them, the next admission refuses such
    /// a task (`owner_published_candidate`), so a requeue without a discard
    /// would only strand it. A claimed or local-only delivery keeps its own
    /// candidate contract, as that admission does: a claim's kept candidate
    /// republishes through its own pull request.
    pub(crate) fn unbound_publication_on_record(&self, task: &Task) -> Result<bool, OrbitError> {
        if let Some(run_id) = task.job_run_id.as_deref() {
            let owner_pr = self.get_job_run_backend(run_id)?.is_some_and(|run| {
                run.job_id == "task_pr_pipeline"
                    && run
                        .input
                        .as_ref()
                        .is_none_or(|input| input["claimed"] != true)
            });
            // A run missing here is a follower's claimed leaf.
            if !owner_pr {
                return Ok(false);
            }
        }
        Ok(task.github_pr_number().is_some()
            || self
                .get_task_history(&task.id)?
                .iter()
                .any(|entry| entry.event == PRESERVED))
    }

    pub(super) fn owner_source_identity(
        &self,
        source: &JobRun,
        task: &Task,
    ) -> Result<(), OrbitError> {
        let local = self
            .automation_machine_identity()
            .ok_or_else(|| refuse("local identity missing"))?;
        if source.job_id != "task_pr_pipeline"
            || source
                .executed_on
                .as_ref()
                .is_none_or(|o| o.machine_id != local)
            || task
                .job_run_machine
                .as_ref()
                .is_none_or(|o| o.machine_id != local)
            || source.input.as_ref().is_none_or(|i| {
                i["claimed"] == true || carried_task_ids(i) != BTreeSet::from([task.id.clone()])
            })
            || self
                .stores()
                .jobs()
                .local_pull_for_run(&source.run_id)?
                .is_some()
            || self
                .resolve_execution_claims()?
                .iter()
                .any(|c| c.claim.task_id == task.id)
        {
            return Err(refuse(
                "published candidate is not an exact owner-local single-task execution",
            ));
        }
        Ok(())
    }

    pub(crate) fn owner_published_candidate(
        &self,
        task_id: &str,
        source_id: Option<&str>,
    ) -> Result<Option<SourceRepairBinding>, OrbitError> {
        let task = self.get_task(task_id)?;
        let current = match &task.job_run_id {
            Some(id) => self.get_job_run_backend(id)?,
            None => None,
        };
        // Claimed and local-only deliveries retain their existing candidate
        // contracts; this path authenticates owner PR publications only.
        if current.as_ref().is_some_and(|run| {
            run.job_id != "task_pr_pipeline"
                || run.input.as_ref().is_some_and(|i| i["claimed"] == true)
        }) {
            return Ok(None);
        }
        let entries = self.get_task_history(task_id)?;
        let records = entries
            .iter()
            .filter(|e| e.event == PRESERVED)
            .map(|e| {
                if e.by != "system" {
                    return Err(refuse("untrusted publication preservation"));
                }
                serde_json::from_str::<PreservedPublication>(e.note.as_deref().unwrap_or_default())
                    .map_err(|e| refuse(e.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let predecessor = || -> Result<String, OrbitError> {
            let current = current
                .as_ref()
                .ok_or_else(|| refuse("current execution missing"))?;
            let runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
                job_id: Some("task_pr_pipeline".into()),
                task_id: Some(task_id.into()),
                limit: Some(2),
                include_steps: false,
                ..Default::default()
            })?;
            if runs.first().is_none_or(|run| run.run_id != current.run_id) {
                return Err(refuse(
                    "newer execution makes publication association ambiguous",
                ));
            }
            Ok(runs
                .get(1)
                .ok_or_else(|| refuse("published predecessor is missing"))?
                .run_id
                .clone())
        };
        // Setup can stamp the task and lose its reply before checkpointing
        // the prior id. Never skip a newer attempt to find an older publication.
        let recovered_source;
        let source_id = match source_id {
            Some(id) => id,
            None if !records.is_empty() || task.github_pr_number().is_some() => {
                recovered_source = predecessor()?;
                &recovered_source
            }
            None => return Ok(None),
        };
        let source = self.get_job_run_backend(source_id)?;
        if source.as_ref().is_some_and(|run| {
            run.job_id.starts_with("task_claimed_")
                || run.input.as_ref().is_some_and(|i| i["claimed"] == true)
        }) {
            return Ok(None);
        }
        let state = self.read_run_state(source_id)?;
        let published = state.as_ref().is_some_and(|s| {
            ["push", "pr_open"]
                .iter()
                .any(|k| s.pipeline.get(k).is_some_and(|v| !v.is_null()))
        });
        let matching = records
            .iter()
            .filter(|r| r.source_run_id == source_id)
            .collect::<Vec<_>>();
        // An operator discarded this run's candidate and no requeue kept its
        // publication: the next run implements fresh, which the generic
        // candidate lookup does once it sees the discard. Only a stopped
        // source may be left behind this way.
        if matching.is_empty()
            && let Some(stopped) = source.as_ref()
            && entries.iter().any(|e| {
                e.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT
                    && e.at >= stopped.created_at
            })
        {
            if stopped.finished_at.is_none()
                || run_owner_liveness(stopped) != RunOwnerLiveness::Stopped
            {
                return Err(refuse(
                    "a discarded publication's source execution is still live",
                ));
            }
            return Ok(None);
        }
        if matching.is_empty() && !published {
            if !records.is_empty() || task.github_pr_number().is_some() {
                return Err(refuse(
                    "a published candidate predates the current binding; inspect explicit source repair with the stopped successor",
                ));
            }
            return Ok(None);
        }
        if matching.len() != 1 {
            return Err(refuse(
                "published candidate lacks a unique requeue binding; inspect explicit source repair",
            ));
        }
        if predecessor()? != source_id {
            return Err(refuse(
                "a newer owner attempt requires explicit source repair",
            ));
        }
        let source = source.ok_or_else(|| refuse("published source missing"))?;
        self.owner_source_identity(&source, &task)?;
        if !matches!(source.state, JobRunState::Failed | JobRunState::Cancelled)
            || source.finished_at.is_none()
            || run_owner_liveness(&source) != RunOwnerLiveness::Stopped
            || self.recorded_final_recovery(task_id, source_id)?
                != Some(crate::application::task::FinalRecoveryOutcome::Requeued)
            || matching[0].schema_version != 1
            || matching[0].task_digest != task_digest(&task)?
            || entries.iter().any(|e| {
                e.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT && e.at >= source.created_at
            })
        {
            return Err(refuse(
                "published source is live, discarded, changed, or lacks an applied requeue",
            ));
        }
        if matching[0].failure_handoff {
            let state = self
                .read_run_state(source_id)?
                .ok_or_else(|| refuse("source checkpoints missing"))?;
            if state.failure_activity_checkpoint.is_none() {
                return Err(refuse("preserved failure-handoff receipt disappeared"));
            }
            let definition = self.source_repair_definition(&source)?;
            return self
                .observe_handoff_publication(source, task, state, definition)
                .map(|(_, _, binding)| Some(binding));
        }
        self.observe_published_source(source, task, false)
            .map(|(_, _, binding)| Some(binding))
    }

    /// Publication authority is the successful host adoption, never a caller's input.
    pub(crate) fn owner_publication_binding(
        &self,
        run_id: &str,
    ) -> Result<Option<SourceRepairBinding>, OrbitError> {
        self.read_owner_publication_binding(run_id, true)
    }

    pub(crate) fn read_owner_publication_binding(
        &self,
        run_id: &str,
        active: bool,
    ) -> Result<Option<SourceRepairBinding>, OrbitError> {
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok(None);
        };
        let Some(state) = self.read_run_state(run_id)? else {
            return Ok(None);
        };
        let Some(output) = state.pipeline.get("resume_candidate") else {
            return Ok(None);
        };
        let Some(raw) = output.get("published_binding") else {
            return Ok(None);
        };
        let binding: SourceRepairBinding =
            serde_json::from_value(raw.clone()).map_err(|e| refuse(e.to_string()))?;
        let task = self.get_task(&binding.task_id)?;
        self.owner_source_identity(&run, &task)?;
        let job = self.source_repair_definition(&run)?;
        let index = job
            .steps
            .iter()
            .position(|s| s.id == "resume_candidate")
            .ok_or_else(|| refuse("adoption step absent"))?;
        let authenticated = matches!(&job.steps[index].body, JobV2StepBody::Target(t) if t.activity_name.as_deref() == Some("candidate_resume") && matches!(&t.spec, ActivityV2Spec::Deterministic(s) if s.action == "candidate_resume"));
        if !authenticated
            || state.run_id != run_id
            || state.job_id != run.job_id
            || state.step_states.get(&(index as u32)) != Some(&JobRunState::Success)
            || state.step_output(index as u32) != Some(output)
            || (active
                && (!matches!(run.state, JobRunState::Pending | JobRunState::Running)
                    || task.job_run_id.as_deref() != Some(run_id)
                    || !matches!(task.status, TaskStatus::InProgress | TaskStatus::Review)))
            || binding.schema_version != 1
            || binding.source_run_id == run_id
            || (active && task_digest(&task)? != binding.task_digest)
            || output["phase"] != "candidate_resume"
            || !matches!(
                output["outcome"].as_str(),
                Some("resumed_carried" | "resumed_repaired")
            )
            || output["source_branch"] != binding.branch
            || output["source_sha"] != binding.head_sha
            || output["base_sha"] != state.pipeline["worktree"]["base_sha"]
            || output["prior_pull_request_head"] != binding.branch
            || output["reused_branch"] != binding.branch
            || output["reused_head_sha"] != binding.head_sha
            || output["prior_pull_request"] != binding.pull_request
            || output["source_run_id"] != binding.source_run_id
        {
            return Err(refuse("published adoption authority changed"));
        }
        let source = self
            .get_job_run_backend(&binding.source_run_id)?
            .ok_or_else(|| refuse("source disappeared"))?;
        let source_state = self
            .read_run_state(&binding.source_run_id)?
            .ok_or_else(|| refuse("source checkpoints disappeared"))?;
        let definition = self.source_repair_definition(&source)?;
        if binding.workspace_id != self.workspace_id()?
            || digest(
                &json!({"run":source,"steps":source.steps,"checkpoints":source_state,"definition":definition}),
            )? != binding.source_digest
        {
            return Err(refuse("immutable published source changed"));
        }
        Ok(Some(binding))
    }
}
