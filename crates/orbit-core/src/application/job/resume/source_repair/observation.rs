//! Authenticate the host commit, review, synchronization and publication chain.
use super::*;

impl OrbitRuntime {
    pub(super) fn observe_published_source(
        &self,
        source: JobRun,
        task: Task,
    ) -> Result<(JobRun, Task, SourceRepairBinding), OrbitError> {
        let source_id = source.run_id.as_str();
        let task_id = task.id.as_str();
        let local = self
            .automation_machine_identity()
            .ok_or_else(|| refuse("local machine identity unavailable"))?;
        let state = self
            .read_run_state(source_id)?
            .ok_or_else(|| refuse("source checkpoints missing"))?;
        if state.run_id != source_id || state.job_id != source.job_id {
            return Err(refuse("source checkpoint identity or claim mismatch"));
        }
        let definition = self.source_repair_definition(&source)?;
        let job = &definition;
        let checkpoint = |id: &str, action: &str| -> Result<&Value, OrbitError> {
            let index = job
                .steps
                .iter()
                .position(|step| step.id == id)
                .ok_or_else(|| refuse(format!("source definition lacks {id}")))?
                as u32;
            if !matches!(&job.steps[index as usize].body,
                JobV2StepBody::Target(target)
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
        };
        let worktree = checkpoint("worktree", "worktree_setup")?;
        let push = checkpoint("push", "git_push")?;
        let pr = checkpoint("pr_open", "pr_open")?;
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
        let number = field(pr, "pr_number")?;
        if adopted.as_ref().is_some_and(|binding| {
            binding.pull_request != number
                || binding.base_branch != base_branch
                || prepared.get("remote_sha").and_then(Value::as_str)
                    != Some(binding.head_sha.as_str())
                || pr.get("pr_url").and_then(Value::as_str)
                    != Some(binding.pull_request_url.as_str())
        }) || field(worktree, "job_run_id")? != source_id
            || field(prepared, "phase")? != "prepare"
            || field(sync, "phase")? != "rebase"
            || field(push, "phase")? != "push"
            || field(pr, "phase")? != "pr_open"
            || field(prepared, "head")? != branch
            || field(sync, "head")? != branch
            || field(push, "branch")? != branch
            || field(pr, "head")? != branch
            || field(prepared, "head_sha")? != field(sync, "head_sha_before")?
            || field(prepared, "base")? != base_branch
            || field(pr, "base")? != base_branch
            || field(prepared, "base_sha")? != base_sha
            || field(pr, "base_sha")? != base_sha
            || field(prepared, "base_ref")? != field(sync, "base_ref")?
            || field(pr, "base_ref")? != field(sync, "base_ref")?
            || prepared.get("remote_sha").is_none()
            || prepared.get("remote_sha") != sync.get("remote_sha_before")
            || push.get("remote_sha_before").is_none()
            || (push.get("remote_sha_before") != sync.get("remote_sha_before")
                && push.get("remote_sha_before").and_then(Value::as_str) != Some(pushed))
        {
            return Err(refuse("task, source branch and published PR disagree"));
        }
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
        if git(&["rev-parse", "--verify", &format!("refs/heads/{branch}")])? != head {
            return Err(refuse("retained local branch moved"));
        }
        if git(&[
            "ls-remote",
            "--exit-code",
            "origin",
            &format!("refs/heads/{branch}"),
        ])?
        .split_whitespace()
        .next()
            != Some(head)
        {
            return Err(refuse("published branch lease drifted or disappeared"));
        }
        let remote_url = git2::Repository::open(repo)
            .and_then(|repo| repo.config()?.get_string("remote.origin.url"))
            .map_err(|e| refuse(format!("origin configuration unreadable: {e}")))?;
        let identity = orbit_types::workspace::git_remote_identity(&remote_url)
            .map_err(|e| refuse(e.to_string()))?;
        let repository = identity
            .strip_prefix("github.com/")
            .ok_or_else(|| refuse("source repair requires an exact GitHub origin identity"))?
            .to_string();
        if adopted.as_ref().is_some_and(|binding| {
            binding.repository != repository || binding.remote_url != remote_url
        }) {
            return Err(refuse("adopted source repository identity changed"));
        }
        let url = format!("https://github.com/{repository}/pull/{number}");
        task_pr_association(&task, number, &url)?;
        let status = self.read_pull_request_status(number)?;
        if status.url.as_deref() != Some(&url)
            || status.number.map(|n| n.to_string()).as_deref() != Some(number)
            || status.state.as_deref() != Some("OPEN")
            || status.head_branch.as_deref() != Some(branch)
            || status.head_sha.as_deref() != Some(head)
            || status.base_branch.as_deref() != Some(base_branch)
            || status.merge_commit.is_some()
            || status.merged_at.is_some()
            || status.auto_merge_enabled != Some(false)
        {
            return Err(refuse(
                "provider PR identity changed, closed/merged, or automatic merge intent is uncertain; inspect it explicitly",
            ));
        }
        // A draft is preserved as draft. Its evidence hold is never released here.
        if field(pr, "pr_url")? != url {
            return Err(refuse("checkpoint PR repository identity disagrees"));
        }
        let binding = SourceRepairBinding {
            schema_version: 1,
            workspace_id: self.workspace_id()?,
            task_id: task_id.into(),
            source_run_id: source_id.into(),
            machine_id: local.to_string(),
            source_digest: digest(
                &json!({"run":source,"steps":source.steps,"checkpoints":state,"definition":definition}),
            )?,
            task_digest: task_digest(&task)?,
            repository,
            remote_url,
            branch: branch.into(),
            pull_request: number.into(),
            pull_request_url: url,
            head_sha: head.into(),
            base_sha: base_sha.into(),
            base_branch: base_branch.into(),
            superseded: None,
        };
        Ok((source, task, binding))
    }
}
