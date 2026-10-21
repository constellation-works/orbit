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
        let checkpoint = |id: &str, action: &str| authenticated_checkpoint(job, &state, id, action);
        if explicit
            && state.failure_activity_checkpoint.is_some()
            && source.state == JobRunState::Failed
        {
            return self.observe_handoff_publication(source, task, state, definition);
        }
        if source.state == JobRunState::Failed
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
        let worktree = checkpoint("worktree", "worktree_setup")?;
        let push = checkpoint("push", "git_push")?;
        let failed_open = explicit
            && source.state == JobRunState::Failed
            && source
                .input
                .as_ref()
                .is_some_and(|i| i.get(SOURCE_REPAIR_KEY).is_some())
            && state.pipeline.get("pr_open").is_none_or(Value::is_null);
        if failed_open {
            let failed = job.steps.iter().enumerate().any(|(index, step)| {
                step.id == "pr_open"
                    && state.step_states.get(&(index as u32)) == Some(&JobRunState::Failed)
                    && matches!(&step.body, JobV2StepBody::Target(t)
                        if t.activity_name.as_deref() == Some("pr_open")
                        && matches!(&t.spec, ActivityV2Spec::Deterministic(s) if s.action == "pr_open"))
            });
            if !failed {
                return Err(refuse(
                    "missing successful PR checkpoint requires an authenticated failed pr_open attempt",
                ));
            }
        }
        let pr = if failed_open {
            None
        } else {
            Some(checkpoint("pr_open", "pr_open")?)
        };
        let sync = checkpoint("sync_base", "git_rebase")?;
        let prepared = checkpoint("prepare_branch", "pr_prepare")?;
        let adopted = if source
            .input
            .as_ref()
            .is_some_and(|input| input.get(SOURCE_REPAIR_KEY).is_some())
        {
            Some(self.source_repair_adopted_binding(
                &source,
                &task,
                worktree,
                checkpoint("resume_candidate", "candidate_resume")?,
            )?)
        } else {
            self.read_owner_publication_binding(source_id, false)?
        };
        // Setup precedes adoption and keeps its per-run branch. Only the
        // authenticated operator continuation may replace that branch identity.
        let branch = match &adopted {
            Some(binding) => binding.branch.as_str(),
            None => field(worktree, "head_ref")?,
        };
        let pushed = field(push, "local_sha")?;
        let synced = field(sync, "head_sha")?;
        let head = publication::published_head(
            job,
            &state,
            &checkpoint,
            sync,
            push,
            branch,
            &self.paths().repo_root,
        )?;
        let base_sha = field(sync, "base_sha")?;
        let base_branch = field(sync, "base")?;
        let number = match pr {
            Some(pr) => field(pr, "pr_number")?,
            None => adopted
                .as_ref()
                .ok_or_else(|| refuse("failed publication lacks original operator admission"))?
                .pull_request
                .as_str(),
        };
        // Orbit's own auto-fix may have published repaired heads on this
        // pull request after the source's push. Follow the chain the source
        // recorded; any other move of the branch is still refused below.
        let advanced =
            auto_fix_advanced_head(&self.get_task_history(task_id)?, source_id, number, head);
        let head = advanced.as_str();
        if adopted.as_ref().is_some_and(|binding| {
            binding.pull_request != number
                || binding.base_branch != base_branch
                || prepared.get("remote_sha").and_then(Value::as_str)
                    != Some(binding.head_sha.as_str())
                || pr.is_some_and(|pr| {
                    pr.get("pr_url").and_then(Value::as_str)
                        != Some(binding.pull_request_url.as_str())
                })
        }) || field(worktree, "job_run_id")? != source_id
            || field(prepared, "phase")? != "prepare"
            || field(sync, "phase")? != "rebase"
            || field(push, "phase")? != "push"
            || pr.is_some_and(|pr| pr.get("phase").and_then(Value::as_str) != Some("pr_open"))
            || field(prepared, "head")? != branch
            || field(sync, "head")? != branch
            || field(push, "branch")? != branch
            || pr.is_some_and(|pr| pr.get("head").and_then(Value::as_str) != Some(branch))
            || field(prepared, "head_sha")? != field(sync, "head_sha_before")?
            || field(prepared, "base")? != base_branch
            || pr.is_some_and(|pr| pr.get("base").and_then(Value::as_str) != Some(base_branch))
            || field(prepared, "base_sha")? != base_sha
            || pr.is_some_and(|pr| pr.get("base_sha").and_then(Value::as_str) != Some(base_sha))
            || field(prepared, "base_ref")? != field(sync, "base_ref")?
            || pr.is_some_and(|pr| pr.get("base_ref") != sync.get("base_ref"))
            || prepared.get("remote_sha").is_none()
            || prepared.get("remote_sha") != sync.get("remote_sha_before")
            || push.get("remote_sha_before").is_none()
            || (push.get("remote_sha_before") != sync.get("remote_sha_before")
                && push.get("remote_sha_before").and_then(Value::as_str) != Some(pushed))
        {
            return Err(refuse("task, source branch and published PR disagree"));
        }
        checkpoint("commit", "git_commit")?;
        let commit = self.observe_task_delivery(task_id, Some(source_id))?.commit;
        if !matches!(
            commit.status,
            CommitObservationStatus::Committed | CommitObservationStatus::AlreadyCommitted
        ) || commit.head_sha.is_none()
            || commit.base_sha.as_deref() != Some(field(worktree, "base_sha")?)
        {
            return Err(refuse("source has no authenticated host commit"));
        }
        let repo = &self.paths().repo_root;
        let git = |args: &[&str]| -> Result<String, OrbitError> {
            let output = orbit_common::fs::git::run_git_within(
                repo,
                args,
                if args.first() == Some(&"ls-remote") {
                    orbit_common::fs::git::GIT_REMOTE_TIMEOUT
                } else {
                    orbit_common::fs::git::GIT_LOCAL_TIMEOUT
                },
            )?;
            if !output.success {
                return Err(refuse(format!("Git observation failed: {}", output.stderr)));
            }
            Ok(output.stdout.trim().to_string())
        };
        for sha in [
            synced,
            pushed,
            head,
            base_sha,
            commit.head_sha.as_deref().unwrap_or_default(),
        ] {
            if sha.len() != 40
                || !sha.bytes().all(|b| b.is_ascii_hexdigit())
                || git(&["rev-parse", "--verify", &format!("{sha}^{{commit}}")])? != sha
            {
                return Err(refuse(
                    "candidate/base/commit object is unavailable or inexact",
                ));
            }
        }
        let committed = commit
            .head_sha
            .as_deref()
            .ok_or_else(|| refuse("commit head missing"))?;
        let before = field(prepared, "head_sha")?;
        // The host may rebase a commit: Git ancestry across that rewrite is
        // intentionally absent. Authenticate the entire successful host chain
        // rather than treating a caller's after-head as provenance.
        if git(&["rev-parse", "--verify", &format!("{before}^{{commit}}")])? != before
            || !run_git(repo, &["merge-base", "--is-ancestor", committed, before])?.success
            || !run_git(repo, &["merge-base", "--is-ancestor", base_sha, synced])?.success
            || sync.get("rewritten").and_then(Value::as_bool) != Some(before != synced)
            || (before != synced
                && prepared.get("sync_required").and_then(Value::as_bool) != Some(true))
        {
            return Err(refuse(
                "authenticated prepare/rebase commit chain is inconsistent",
            ));
        }
        let binding = self.bind_source_publication(
            source.clone(),
            task.clone(),
            state.clone(),
            definition.clone(),
            super::identity::PublicationIdentity {
                branch,
                head,
                base_sha,
                base_branch,
                number,
                allow_missing_branch: false,
            },
        )?;
        if pr.is_some_and(|pr| {
            pr.get("pr_url").and_then(Value::as_str) != Some(binding.2.pull_request_url.as_str())
        }) || adopted.as_ref().is_some_and(|prior| {
            prior.repository != binding.2.repository || prior.remote_url != binding.2.remote_url
        }) {
            return Err(refuse("checkpoint PR repository identity disagrees"));
        }
        Ok(binding)
    }
}

/// The head `published` reaches through the repaired heads `source_id`
/// recorded as `pr_auto_fix_pushed` on pull request `number`, each one taken
/// only from the head before it. A push another run or actor recorded, or one
/// that does not continue the chain, is not followed.
fn auto_fix_advanced_head(
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
