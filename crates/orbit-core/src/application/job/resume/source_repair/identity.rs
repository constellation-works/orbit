//! Fresh provider and Git identity shared by the two publication provenance paths.
use super::*;

pub(super) struct PublicationIdentity<'a> {
    pub(super) branch: &'a str,
    pub(super) head: &'a str,
    pub(super) base_sha: &'a str,
    pub(super) base_branch: &'a str,
    pub(super) number: &'a str,
    pub(super) allow_missing_branch: bool,
}

impl OrbitRuntime {
    pub(super) fn bind_source_publication(
        &self,
        source: JobRun,
        task: Task,
        state: PipelineState,
        definition: JobV2,
        identity: PublicationIdentity<'_>,
    ) -> Result<(JobRun, Task, SourceRepairBinding), OrbitError> {
        let PublicationIdentity {
            branch,
            head,
            base_sha,
            base_branch,
            number,
            allow_missing_branch,
        } = identity;
        let repo = &self.paths().repo_root;
        let git = |args: &[&str]| -> Result<String, OrbitError> {
            let result = orbit_common::fs::git::run_git_within(
                repo,
                args,
                if args.first() == Some(&"ls-remote") {
                    orbit_common::fs::git::GIT_REMOTE_TIMEOUT
                } else {
                    orbit_common::fs::git::GIT_LOCAL_TIMEOUT
                },
            )?;
            if !result.success {
                return Err(refuse(format!("Git observation failed: {}", result.stderr)));
            }
            Ok(result.stdout.trim().into())
        };
        for sha in [head, base_sha] {
            if sha.len() != 40
                || !sha.bytes().all(|b| b.is_ascii_hexdigit())
                || git(&["rev-parse", "--verify", &format!("{sha}^{{commit}}")])? != sha
            {
                return Err(refuse("candidate/base object unavailable or inexact"));
            }
        }
        let local_ref = format!("refs/heads/{branch}");
        match git2::Repository::open(repo)
            .and_then(|r| r.find_reference(&local_ref).map(|r| r.target()))
        {
            Ok(Some(oid)) if oid.to_string() == head => {}
            Err(error) if allow_missing_branch && error.code() == git2::ErrorCode::NotFound => {}
            _ => return Err(refuse("retained local branch moved")),
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
        let url = format!("https://github.com/{repository}/pull/{number}");
        task_pr_association(&task, number, &url)?;
        let status = self.read_pull_request_status(number)?;
        if status.url.as_deref() != Some(&url)
            || status.number.map(|n| n.to_string()).as_deref() != Some(number)
            || status.state.as_deref() != Some("OPEN")
            || status.head_branch.as_deref() != Some(branch)
            || status.head_sha.as_deref() != Some(head)
            || status.base_branch.as_deref() != Some(base_branch)
            || !status.merge_absence_confirmed()
            || status.merge_commit.is_some()
            || status.merged_at.is_some()
            || status.auto_merge_enabled != Some(false)
        {
            return Err(refuse(
                "provider PR identity changed, closed/merged, or automatic merge intent is uncertain; inspect it explicitly",
            ));
        }
        let binding = SourceRepairBinding {
            schema_version: 1,
            workspace_id: self.workspace_id()?,
            task_id: task.id.clone(),
            source_run_id: source.run_id.clone(),
            machine_id: self
                .automation_machine_identity()
                .ok_or_else(|| refuse("local identity missing"))?
                .into(),
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
            checkpoint_owner: None,
        };
        Ok((source, task, binding))
    }
}
