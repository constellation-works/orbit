//! Read-only proof shared by publication recovery and desktop review.
use super::observation::{authenticated_checkpoint, auto_fix_advanced_head};
use super::*;

/// Authenticated publication identity, independent of recovery admission.
pub(crate) struct NativePublicationProof {
    pub(crate) branch: String,
    pub(crate) head: String,
    pub(crate) base_sha: String,
    pub(crate) base_branch: String,
    pub(crate) number: String,
    pub(crate) url: Option<String>,
    pub(super) adopted: Option<SourceRepairBinding>,
}

impl OrbitRuntime {
    /// Observe only the exact native execution the task names. A local row
    /// with a foreign run's id never grants publication authority.
    pub(crate) fn desktop_native_publication_proof(
        &self,
        task: &Task,
    ) -> Result<NativePublicationProof, OrbitError> {
        let run_id = task
            .job_run_id
            .as_deref()
            .ok_or_else(|| refuse("native run binding missing"))?;
        let machine = task
            .job_run_machine
            .as_ref()
            .ok_or_else(|| refuse("native machine binding missing"))?;
        if self.automation_machine_identity() != Some(machine.machine_id.as_str()) {
            return Err(refuse("native machine binding disagrees"));
        }
        let source = self.show_job_run_observed(run_id)?;
        if source.run_id != run_id
            || source.state != JobRunState::Success
            || source.finished_at.is_none()
            || run_owner_liveness(&source) != RunOwnerLiveness::Stopped
            || source
                .executed_on
                .as_ref()
                .is_none_or(|origin| origin.machine_id != machine.machine_id)
            || source
                .input
                .as_ref()
                .is_some_and(|input| input["claimed"] == true)
            || self.stores().jobs().local_pull_for_run(run_id)?.is_some()
            || !source
                .input
                .as_ref()
                .map(carried_task_ids)
                .unwrap_or_default()
                .contains(&task.id)
        {
            return Err(refuse(
                "native publication requires the task's successful bound run",
            ));
        }
        let state = self
            .read_run_state(run_id)?
            .ok_or_else(|| refuse("native publication checkpoints missing"))?;
        if state.run_id != run_id || state.job_id != source.job_id {
            return Err(refuse("native publication checkpoint identity disagrees"));
        }
        let job = self.source_repair_definition(&source)?;
        for id in [
            "worktree",
            "commit",
            "prepare_branch",
            "sync_base",
            "push",
            "pr_open",
        ] {
            if job.steps.iter().filter(|step| step.id == id).count() != 1
                || !source
                    .steps
                    .iter()
                    .rev()
                    .find(|step| step.target_id == id)
                    .is_some_and(|step| {
                        step.state == JobRunState::Success
                            && step.target_type == orbit_types::workflow::JobTargetType::Activity
                    })
            {
                return Err(refuse(
                    "native publication is missing an unambiguous successful host step",
                ));
            }
        }
        self.authenticate_publication_chain(&source, task, &state, &job, false)
    }

    pub(super) fn authenticate_publication_chain(
        &self,
        source: &JobRun,
        task: &Task,
        state: &PipelineState,
        job: &JobV2,
        explicit: bool,
    ) -> Result<NativePublicationProof, OrbitError> {
        let source_id = source.run_id.as_str();
        let task_id = task.id.as_str();
        let checkpoint = |id: &str, action: &str| authenticated_checkpoint(job, state, id, action);
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
                source,
                task,
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
            state,
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
        Ok(NativePublicationProof {
            branch: branch.into(),
            head: head.into(),
            base_sha: base_sha.into(),
            base_branch: base_branch.into(),
            number: number.into(),
            url: pr
                .map(|pr| field(pr, "pr_url").map(String::from))
                .transpose()?,
            adopted,
        })
    }
}
