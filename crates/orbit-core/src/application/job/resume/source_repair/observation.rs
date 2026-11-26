//! Authenticate the host commit, review, synchronization and publication chain.
use super::*;
use orbit_types::plugin::is_sha1_commit_id;
use orbit_types::workflow::{SuccessfulCheckpoint, pr_auto_fix_pushes};

const MAX_HELD_RETRY_HOPS: usize = 64;

impl OrbitRuntime {
    pub(super) fn observe_published_source(
        &self,
        source: JobRun,
        task: Task,
        explicit: bool,
    ) -> Result<(JobRun, Task, SourceRepairBinding), OrbitError> {
        self.observe_publication_checkpoints(source, task, explicit, false, None)
    }

    pub(super) fn observe_held_retry_source(
        &self,
        source: JobRun,
        task: Task,
        lineage: &[String],
    ) -> Result<(JobRun, Task, SourceRepairBinding), OrbitError> {
        self.observe_publication_checkpoints(source, task, true, false, Some(lineage))
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
        self.observe_publication_checkpoints(source, task, false, true, None)
    }

    /// The stopped owner-local completion-retry chain `[owner, ..., source]`
    /// when `source` is a conclusively stopped `Held` run that reused
    /// `task.job_run_id`'s published checkpoints without a `failure_activity_checkpoint`.
    pub(super) fn held_completion_retry_lineage(
        &self,
        source: &JobRun,
        task: &Task,
    ) -> Result<Option<Vec<String>>, OrbitError> {
        let Some(owner_id) = task.job_run_id.as_deref() else {
            return Ok(None);
        };
        if source.state != JobRunState::Held
            || owner_id == source.run_id
            || source.retry_source_run_id.is_none()
        {
            return Ok(None);
        }
        let Some(source_state) = self.read_run_state(&source.run_id)? else {
            return Ok(None);
        };
        if source_state.run_id != source.run_id
            || source_state.job_id != source.job_id
            || source_state.failure_activity_checkpoint.is_some()
            || source_state
                .pipeline
                .get("worktree")
                .and_then(|v| v.get("job_run_id"))
                .and_then(Value::as_str)
                != Some(owner_id)
            || source_state.pipeline.get("push").is_none_or(Value::is_null)
            || source_state
                .pipeline
                .get("pr_open")
                .is_none_or(Value::is_null)
        {
            return Ok(None);
        }
        let Some(local) = self.automation_machine_identity() else {
            return Ok(None);
        };
        let mut chain = vec![source.run_id.clone()];
        let mut seen = BTreeSet::from([source.run_id.clone()]);
        let mut child = source.clone();
        let owner = loop {
            if chain.len() > MAX_HELD_RETRY_HOPS {
                return Err(refuse("held completion retry lineage is unbounded"));
            }
            if child
                .input
                .as_ref()
                .is_some_and(|i| i.get(SOURCE_REPAIR_KEY).is_some())
            {
                return Ok(None);
            }
            let Some(parent_id) = child.retry_source_run_id.as_deref() else {
                return Ok(None);
            };
            if !seen.insert(parent_id.to_string()) {
                return Err(refuse("held completion retry lineage repeats a run"));
            }
            let parent = self
                .get_job_run_backend(parent_id)?
                .ok_or_else(|| refuse("held completion retry ancestor is missing"))?;
            if parent.job_id != orbit_types::workflow::TASK_PR_PIPELINE_JOB
                || parent.created_at >= child.created_at
                || !parent.state.is_terminal()
                || parent.finished_at.is_none()
                || run_owner_liveness(&parent) != RunOwnerLiveness::Stopped
                || parent
                    .executed_on
                    .as_ref()
                    .is_none_or(|o| o.machine_id != local)
                || self
                    .stores()
                    .jobs()
                    .local_pull_for_run(parent_id)?
                    .is_some()
                || parent.input.as_ref().is_none_or(|i| {
                    i.get("claimed").and_then(Value::as_bool) == Some(true)
                        || carried_task_ids(i) != BTreeSet::from([task.id.clone()])
                })
            {
                return Err(refuse(
                    "held completion retry lineage contains an active, foreign or non-matching run",
                ));
            }
            chain.push(parent.run_id.clone());
            if parent.run_id == owner_id {
                break parent;
            }
            let parent_state = self
                .read_run_state(parent_id)?
                .ok_or_else(|| refuse("held completion retry ancestor checkpoints missing"))?;
            if parent_state.failure_activity_checkpoint.is_some() {
                return Ok(None);
            }
            child = parent;
        };
        let owner_state = self
            .read_run_state(&owner.run_id)?
            .ok_or_else(|| refuse("checkpoint owner state is missing"))?;
        if owner_state.run_id != owner.run_id
            || owner_state.job_id != owner.job_id
            || owner_state.failure_activity_checkpoint.is_some()
        {
            return Ok(None);
        }
        for key in [
            "worktree",
            "commit",
            "prepare_branch",
            "sync_base",
            "push",
            "pr_open",
        ] {
            if source_state.pipeline.get(key) != owner_state.pipeline.get(key) {
                return Err(refuse(
                    "held completion retry publication checkpoints disagree with owner",
                ));
            }
        }
        for key in [
            "resume_candidate",
            "review_gate_settle",
            "review_validate",
            "landing_review_gate_settle",
            "landing_review_validate",
            "landing_push",
        ] {
            if source_state.pipeline.get(key).is_some_and(|v| !v.is_null())
                && source_state.pipeline.get(key) != owner_state.pipeline.get(key)
            {
                return Err(refuse(
                    "held completion retry review checkpoints disagree with owner",
                ));
            }
        }
        chain.reverse();
        Ok(Some(chain))
    }

    fn observe_publication_checkpoints(
        &self,
        source: JobRun,
        task: Task,
        explicit: bool,
        escalation_requeue: bool,
        held_retry_lineage: Option<&[String]>,
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
        let proof = self.authenticate_publication_chain(
            &source,
            &task,
            &state,
            job,
            explicit,
            held_retry_lineage,
        )?;
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
                preserved_review_tip: proof.preserved_review_tip.as_ref(),
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
    let output = run_git_within(&orbit_common::Env::capture(), repo, args, timeout)?;
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
    Ok(matches!(
        orbit_common::fs::git::git_ancestry(
            &orbit_common::Env::capture(),
            repo,
            ancestor,
            descendant
        )?,
        orbit_common::fs::git::GitAncestry::Ancestor
    ))
}

/// Verify that the supplied name resolves exactly to the same commit.
pub(super) fn resolves_to_commit(repo: &std::path::Path, sha: &str) -> Result<bool, OrbitError> {
    Ok(read_git_observation(
        repo,
        &["rev-parse", "--verify", &format!("{sha}^{{commit}}")],
        false,
    )? == sha)
}

/// The head `published` reaches through the repaired heads any run in `lineage`
/// recorded as `pr_auto_fix_pushed` on pull request `number`, each one taken
/// only from the head before it. A push another run or actor recorded, or one
/// that does not continue the chain, is not followed.
pub(super) fn auto_fix_advanced_head(
    history: &[TaskHistoryEntry],
    lineage: &[String],
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
        if lineage.iter().any(|run_id| run_id == &push.run_id) && push.from_head_sha == head {
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
