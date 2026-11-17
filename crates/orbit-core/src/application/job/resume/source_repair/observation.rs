//! Authenticate the host commit, review, synchronization and publication chain.
use super::*;
use orbit_types::plugin::is_sha1_commit_id;
use orbit_types::workflow::{SuccessfulCheckpoint, pr_auto_fix_pushes};

/// Whether a source-repair identity is a full SHA-1 object name.
pub(super) fn is_exact_sha(sha: &str) -> bool {
    is_sha1_commit_id(sha)
}

/// Read Git output with the deadline and refusal appropriate to the observation.
pub(super) fn read_git_observation(
    repo: &std::path::Path,
    args: &[&str],
    completion: bool,
) -> Result<String, OrbitError> {
    use orbit_common::fs::git::{GIT_LOCAL_TIMEOUT, GIT_REMOTE_TIMEOUT, run_git_within};
    let timeout = if args.first() == Some(&"ls-remote") {
        GIT_REMOTE_TIMEOUT
    } else {
        GIT_LOCAL_TIMEOUT
    };
    let output = run_git_within(repo, args, timeout)?;
    if !output.success {
        return Err(if completion {
            refuse("completion Git observation failed")
        } else {
            refuse(format!("Git observation failed: {}", output.stderr))
        });
    }
    Ok(output.stdout.trim().to_string())
}

/// Non-success Git exits, including errors, never establish ancestry.
pub(super) fn is_git_ancestor(
    repo: &std::path::Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool, OrbitError> {
    Ok(orbit_common::fs::git::run_git(
        repo,
        &["merge-base", "--is-ancestor", ancestor, descendant],
    )?
    .success)
}

/// Verify that the supplied name resolves exactly to the same commit.
pub(super) fn resolves_to_commit(repo: &std::path::Path, sha: &str) -> Result<bool, OrbitError> {
    Ok(read_git_observation(
        repo,
        &["rev-parse", "--verify", &format!("{sha}^{{commit}}")],
        false,
    )? == sha)
}

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
                certified_tip: proof.certified_tip.as_ref(),
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
    if !matches!(
        state.successful_checkpoint(index, id),
        SuccessfulCheckpoint::Recorded(_)
    ) {
        return Err(refuse(format!(
            "{id} checkpoint is not authenticated success"
        )));
    }
    Ok(output)
}
