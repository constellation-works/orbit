//! Authenticate the host commit, review, synchronization and publication chain.
use super::*;
use orbit_types::workflow::pr_auto_fix_pushes;

impl OrbitRuntime {
    pub(super) fn observe_published_source(
        &self,
        source: JobRun,
        task: Task,
        explicit: bool,
    ) -> Result<(JobRun, Task, SourceRepairBinding), OrbitError> {
        self.observe_publication_checkpoints(source, task, explicit, false)
    }

    /// [`Self::observe_published_source`] for an operator requeue of the
    /// escalation `source`'s final recovery recorded. The requeue
    /// authenticates the run's completion-failure receipt first and records
    /// the `requeued` decision that supersedes the escalation, so the next
    /// run's adoption passes the guard this skips.
    pub(super) fn observe_escalated_publication(
        &self,
        source: JobRun,
        task: Task,
    ) -> Result<(JobRun, Task, SourceRepairBinding), OrbitError> {
        self.observe_publication_checkpoints(source, task, false, true)
    }

    fn observe_publication_checkpoints(
        &self,
        source: JobRun,
        task: Task,
        explicit: bool,
        escalation_requeue: bool,
    ) -> Result<(JobRun, Task, SourceRepairBinding), OrbitError> {
        let source_id = source.run_id.as_str();
        let task_id = task.id.as_str();
        let state = self
            .read_run_state(source_id)?
            .ok_or_else(|| refuse("source checkpoints missing"))?;
        if state.run_id != source_id || state.job_id != source.job_id {
            return Err(refuse("source checkpoint identity or claim mismatch"));
        }
        let definition = self.source_repair_definition(&source)?;
        let job = &definition;
        let inline = explicit
            && source.state == JobRunState::Failed
            && inline_publication::kept_inline_publication(&state);
        if explicit
            && state.failure_activity_checkpoint.is_some()
            && source.state == JobRunState::Failed
        {
            if inline {
                inline_publication::authenticate_kept_inline_receipt(&source, &task, &state, job)?;
            } else {
                return self.observe_handoff_publication(source, task, state, definition);
            }
        }
        if !escalation_requeue
            && !inline
            && source.state == JobRunState::Failed
            && matches!(
                self.recorded_final_recovery(task_id, source_id)?,
                Some(crate::application::task::FinalRecoveryOutcome::Escalated { .. })
            )
            && source
                .input
                .as_ref()
                .is_none_or(|i| i.get(SOURCE_REPAIR_KEY).is_none())
        {
            return Err(refuse(
                "stopped escalated publication requires original operator admission or authenticated failure-handoff evidence",
            ));
        }
        let proof = self.authenticate_publication_chain(&source, &task, &state, job, explicit)?;
        let binding = self.bind_source_publication(
            source.clone(),
            task.clone(),
            state.clone(),
            definition.clone(),
            super::identity::PublicationIdentity {
                branch: &proof.branch,
                head: &proof.head,
                base_sha: &proof.base_sha,
                base_branch: &proof.base_branch,
                number: &proof.number,
                allow_missing_branch: false,
            },
        )?;
        if proof
            .url
            .as_deref()
            .is_some_and(|url| url != binding.2.pull_request_url)
            || proof.adopted.as_ref().is_some_and(|prior| {
                prior.repository != binding.2.repository || prior.remote_url != binding.2.remote_url
            })
        {
            return Err(refuse("checkpoint PR repository identity disagrees"));
        }
        if inline {
            let receipt = &state
                .failure_activity_checkpoint
                .as_ref()
                .ok_or_else(|| refuse("kept publication receipt missing"))?
                .output;
            if field(receipt, "pr_number")? != binding.2.pull_request
                || field(receipt, "pr_url")? != binding.2.pull_request_url
            {
                return Err(refuse(
                    "kept publication receipt contradicts original PR proof",
                ));
            }
        }
        Ok(binding)
    }
}

/// The head `published` reaches through the repaired heads `source_id`
/// recorded as `pr_auto_fix_pushed` on pull request `number`, each one taken
/// only from the head before it. A push another run or actor recorded, or one
/// that does not continue the chain, is not followed.
pub(super) fn auto_fix_advanced_head(
    history: &[TaskHistoryEntry],
    source_id: &str,
    number: &str,
    published: &str,
) -> String {
    let recorded = history
        .iter()
        .filter(|entry| entry.by == "system")
        .cloned()
        .collect::<Vec<_>>();
    let mut head = published.to_string();
    for push in pr_auto_fix_pushes(&recorded, number) {
        if push.run_id == source_id && push.from_head_sha == head {
            head = push.to_head_sha;
        }
    }
    head
}

pub(super) fn authenticated_checkpoint<'a>(
    job: &JobV2,
    state: &'a PipelineState,
    id: &str,
    action: &str,
) -> Result<&'a Value, OrbitError> {
    let index = job
        .steps
        .iter()
        .position(|step| step.id == id)
        .ok_or_else(|| refuse(format!("source definition lacks {id}")))? as u32;
    if !matches!(&job.steps[index as usize].body, JobV2StepBody::Target(target)
        if target.activity_name.as_deref() == Some(action)
        && matches!(&target.spec, ActivityV2Spec::Deterministic(spec) if spec.action == action))
    {
        return Err(refuse(format!(
            "{id} is not the authenticated host {action} action"
        )));
    }
    let output = state
        .pipeline
        .get(id)
        .ok_or_else(|| refuse(format!("source lacks {id} evidence")))?;
    if state.step_states.get(&index) != Some(&JobRunState::Success)
        || state.step_output(index) != Some(output)
    {
        return Err(refuse(format!(
            "{id} checkpoint is not authenticated success"
        )));
    }
    Ok(output)
}
