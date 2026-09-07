use std::fs;
use std::path::Path;
use std::process::Output;

use serde_json::Value;

use crate::context::{RuntimeHost, StepRecoveryAdmission};
use crate::executor::automation::vcs::git::git_command;

use super::{
    DispatchError, GitWorktreeFingerprint, WorktreeBoundaryGuard, changed_paths, git_command_error,
    git_fingerprint, git_output_raw, git_stdout, git_stdout_bytes, nul_paths, safe_relative_path,
};

pub(super) struct RebaseRecoveryCheckpoint {
    branch: String,
    original_head: String,
    original_base_sha: String,
    base_ref: String,
    target_base_sha: String,
    conflicting_paths: Vec<String>,
    remote_sha_before: Option<String>,
}

impl WorktreeBoundaryGuard {
    /// Admit file repair for the already stopped, checkpoint-matching rebase.
    /// The provider remains unable to write Git metadata; after it exits, the
    /// host boundary independently validates and completes this checkpoint.
    pub(crate) fn authorize_rebase_completion(
        &mut self,
        input: &Value,
    ) -> Result<(), DispatchError> {
        let invalid = || {
            DispatchError::CliInvocationPermanent(
                "conflict recovery requires an existing rebase matching the prepared branch, \
                 original HEAD and pinned target base"
                    .to_string(),
            )
        };
        if input.get("recovery_kind").and_then(Value::as_str) != Some("vcs_conflict")
            || input.get("operation").and_then(Value::as_str) != Some("git_rebase")
        {
            return Err(invalid());
        }
        let prepared = input.get("failed_step_input").ok_or_else(invalid)?;
        let branch = prepared
            .get("head")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let original = prepared
            .get("head_sha")
            .or_else(|| prepared.get("published_head_sha"))
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let original_base_sha = input
            .get("original_base_sha")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let target = input
            .get("target_base_sha")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let base_ref = prepared
            .get("base_ref")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| completion_recovery_base_ref(prepared))
            .ok_or_else(invalid)?;
        let prepared_target = prepared
            .get("base_sha")
            .and_then(Value::as_str)
            .unwrap_or(target);
        validate_failed_step_identity(input, prepared, original).ok_or_else(invalid)?;
        if prepared_target != target
            || git_stdout(&self.assigned_root, &["rev-parse", "--verify", &base_ref])? != target
            || git_stdout(&self.assigned_root, &["merge-base", original, target])?
                != original_base_sha
        {
            return Err(invalid());
        }
        let conflicting_paths = recovery_conflicting_paths(input).ok_or_else(invalid)?;
        if unmerged_paths(&self.assigned_root)? != conflicting_paths {
            return Err(invalid());
        }
        let expected_ref = format!("refs/heads/{branch}");
        if git_stdout(
            &self.assigned_root,
            &["rev-parse", "--verify", &expected_ref],
        )? != original
        {
            return Err(invalid());
        }
        for backend in ["rebase-merge", "rebase-apply"] {
            let path = git_stdout(
                &self.assigned_root,
                &["rev-parse", "--path-format=absolute", "--git-path", backend],
            )?;
            let path = Path::new(&path);
            if !path.is_dir() {
                continue;
            }
            let read = |name: &str| {
                fs::read_to_string(path.join(name)).map(|text| text.trim().to_string())
            };
            if read("head-name").ok().as_deref() != Some(expected_ref.as_str())
                || read("orig-head").ok().as_deref() != Some(original)
                || read("onto").ok().as_deref() != Some(target)
            {
                return Err(invalid());
            }
            self.rebase_recovery = Some(RebaseRecoveryCheckpoint {
                branch: branch.to_string(),
                original_head: original.to_string(),
                original_base_sha: original_base_sha.to_string(),
                base_ref,
                target_base_sha: target.to_string(),
                conflicting_paths,
                remote_sha_before: prepared
                    .get("remote_sha")
                    .or_else(|| prepared.get("published_head_sha"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
            return Ok(());
        }
        Err(invalid())
    }

    pub(super) fn completed_authorized_rebase(
        &self,
        after: &GitWorktreeFingerprint,
    ) -> Result<bool, DispatchError> {
        let Some(checkpoint) = &self.rebase_recovery else {
            return Ok(false);
        };
        if after.branch.as_deref() != Some(checkpoint.branch.as_str()) {
            return Ok(false);
        }
        for backend in ["rebase-merge", "rebase-apply"] {
            let path = git_stdout(
                &self.assigned_root,
                &["rev-parse", "--path-format=absolute", "--git-path", backend],
            )?;
            if Path::new(&path).exists() {
                return Ok(false);
            }
        }
        Ok(after.head != checkpoint.target_base_sha
            && git_output_raw(
                &self.assigned_root,
                &[
                    "merge-base",
                    "--is-ancestor",
                    &checkpoint.target_base_sha,
                    "HEAD",
                ],
            )?
            .status
            .success())
    }

    pub(super) fn complete_rebase_recovery(
        &self,
        host: &dyn RuntimeHost,
        step_id: &str,
        task_ids: &[String],
    ) -> Result<Value, DispatchError> {
        let checkpoint = self.rebase_recovery.as_ref().ok_or_else(|| {
            DispatchError::CliInvocationPermanent(
                "conflict recovery has no authenticated rebase checkpoint".to_string(),
            )
        })?;
        let invalid = |reason: &str| {
            DispatchError::CliInvocationPermanent(format!(
                "conflict recovery refused host Git continuation: {reason}"
            ))
        };

        let after_agent = git_fingerprint(&self.assigned_root)?;
        if after_agent.head != self.assigned_before.head
            || after_agent.branch != self.assigned_before.branch
            || after_agent.index_sha256 != self.assigned_before.index_sha256
        {
            return Err(invalid(
                "the provider changed HEAD, branch, or index instead of only repairing files",
            ));
        }
        let changed = changed_paths(&self.assigned_root, &self.assigned_before, &after_agent);
        if changed
            .iter()
            .any(|path| !checkpoint.conflicting_paths.contains(path))
        {
            return Err(invalid(
                "the provider changed a path outside the authorized conflict set",
            ));
        }
        if checkpoint
            .conflicting_paths
            .iter()
            .any(|path| !changed.contains(path))
        {
            return Err(invalid(
                "one or more authorized conflict files were not repaired",
            ));
        }
        self.validate_rebase_checkpoint(checkpoint, &invalid)?;
        if unmerged_paths(&self.assigned_root)? != checkpoint.conflicting_paths {
            return Err(invalid(
                "the live unmerged paths changed after provider execution",
            ));
        }

        let mut diff_check_args = vec!["diff", "--check", "--"];
        diff_check_args.extend(checkpoint.conflicting_paths.iter().map(String::as_str));
        let diff_check = git_output_raw(&self.assigned_root, &diff_check_args)?;
        if !diff_check.status.success() {
            return Err(invalid(
                "a repaired file still contains conflict markers or whitespace errors",
            ));
        }

        host.validate_step_recovery_mutation(&self.run_id, step_id, task_ids, &self.assigned_root)
            .map_err(|error| {
                invalid(&format!(
                    "live run/task/worktree ownership changed: {error}"
                ))
            })?;
        if let StepRecoveryAdmission::Denied { reason } = host
            .authorize_step_recovery(&self.run_id, step_id)
            .map_err(|error| invalid(&format!("recovery authorization recheck failed: {error}")))?
        {
            return Err(invalid(&format!(
                "recovery authorization was revoked: {reason}"
            )));
        }

        let mut add_args = vec!["add", "--"];
        add_args.extend(checkpoint.conflicting_paths.iter().map(String::as_str));
        git_mutation(&self.assigned_root, &add_args)?;
        if !unmerged_paths(&self.assigned_root)?.is_empty() {
            return Err(invalid(
                "staging the authorized conflict set left unresolved entries",
            ));
        }
        let continued = git_mutation_output(
            &self.assigned_root,
            &["-c", "core.editor=true", "rebase", "--continue"],
        )?;
        if !continued.status.success() {
            let additional = unmerged_paths(&self.assigned_root)?;
            let diagnostic = String::from_utf8_lossy(&continued.stderr)
                .trim()
                .to_string();
            return Err(invalid(&format!(
                "rebase --continue failed; additional_conflicting_paths={additional:?}; diagnostic={diagnostic}"
            )));
        }

        let completed = git_fingerprint(&self.assigned_root)?;
        if !self.completed_authorized_rebase(&completed)? {
            return Err(invalid(
                "continued rebase did not leave the checkpointed branch on the pinned base with a candidate commit",
            ));
        }
        // The stopped index includes every nonconflicting candidate change.
        // Those staged paths become clean when Git commits them; comparing dirty
        // path maps across that transition incorrectly rejects the candidate.
        // Provider edits were checked before continuation. Now require Git to
        // leave no tracked dirt and preserve the pre-existing untracked payload.
        if !git_output_raw(&self.assigned_root, &["diff", "--quiet", "HEAD", "--"])?
            .status
            .success()
            || completed.untracked_content != self.assigned_before.untracked_content
        {
            return Err(invalid(
                "host continuation left tracked dirt or changed untracked files",
            ));
        }
        Ok(serde_json::json!({
            "run_id": self.run_id,
            "step_id": step_id,
            "task_ids": task_ids,
            "workspace_path": self.assigned_root,
            "head": checkpoint.branch,
            "head_sha_before": checkpoint.original_head,
            "original_base_sha": checkpoint.original_base_sha,
            "base_ref": checkpoint.base_ref,
            "base_sha": checkpoint.target_base_sha,
            "remote_sha_before": checkpoint.remote_sha_before,
            "head_sha": completed.head,
            "rewritten": true,
        }))
    }

    fn validate_rebase_checkpoint(
        &self,
        checkpoint: &RebaseRecoveryCheckpoint,
        invalid: &impl Fn(&str) -> DispatchError,
    ) -> Result<(), DispatchError> {
        let expected_ref = format!("refs/heads/{}", checkpoint.branch);
        if git_stdout(
            &self.assigned_root,
            &["rev-parse", "--verify", &expected_ref],
        )? != checkpoint.original_head
            || git_stdout(
                &self.assigned_root,
                &["rev-parse", "--verify", &checkpoint.base_ref],
            )? != checkpoint.target_base_sha
            || git_stdout(
                &self.assigned_root,
                &[
                    "merge-base",
                    &checkpoint.original_head,
                    &checkpoint.target_base_sha,
                ],
            )? != checkpoint.original_base_sha
        {
            return Err(invalid(
                "branch, original HEAD, original base, or pinned base moved",
            ));
        }
        for backend in ["rebase-merge", "rebase-apply"] {
            let path = git_stdout(
                &self.assigned_root,
                &["rev-parse", "--path-format=absolute", "--git-path", backend],
            )?;
            let path = Path::new(&path);
            if !path.is_dir() {
                continue;
            }
            let read = |name: &str| {
                fs::read_to_string(path.join(name)).map(|text| text.trim().to_string())
            };
            if read("head-name").ok().as_deref() == Some(expected_ref.as_str())
                && read("orig-head").ok().as_deref() == Some(checkpoint.original_head.as_str())
                && read("onto").ok().as_deref() == Some(checkpoint.target_base_sha.as_str())
            {
                return Ok(());
            }
        }
        Err(invalid(
            "the stopped rebase metadata no longer matches its checkpoint",
        ))
    }
}

fn validate_failed_step_identity(input: &Value, prepared: &Value, original: &str) -> Option<()> {
    let activity_name = input.get("activity_name")?.as_str()?;
    let failed_step_id = input.get("failed_step_id")?.as_str()?;
    if !matches!(
        (failed_step_id, activity_name),
        ("sync_base", "git_rebase") | ("complete_pr", "pr_complete")
    ) {
        return None;
    }
    if activity_name == "pr_complete"
        && (prepared.get("completion")?.as_str()? != "done"
            || prepared.get("published_head_sha")?.as_str()? != original
            || prepared.get("pr_number")?.as_str()?.is_empty())
    {
        return None;
    }
    Some(())
}

fn recovery_conflicting_paths(input: &Value) -> Option<Vec<String>> {
    let values = input.get("conflicting_paths")?.as_array()?;
    let mut paths = Vec::with_capacity(values.len());
    for value in values {
        let path = value.as_str()?;
        safe_relative_path(path).ok()?;
        paths.push(path.to_string());
    }
    if paths.is_empty() {
        return None;
    }
    let original_len = paths.len();
    paths.sort();
    paths.dedup();
    (paths.len() == original_len).then_some(paths)
}

fn completion_recovery_base_ref(prepared: &Value) -> Option<String> {
    let base = prepared.get("base")?.as_str()?;
    match prepared.get("base_sync").and_then(Value::as_str) {
        Some("local") => Some(base.to_string()),
        Some("remote") | None => Some(format!(
            "refs/remotes/origin/{}",
            base.strip_prefix("origin/").unwrap_or(base)
        )),
        Some(_) => None,
    }
}

fn unmerged_paths(root: &Path) -> Result<Vec<String>, DispatchError> {
    let mut paths = nul_paths(&git_stdout_bytes(
        root,
        &["diff", "--name-only", "-z", "--diff-filter=U", "--"],
    )?);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn git_mutation(root: &Path, args: &[&str]) -> Result<(), DispatchError> {
    let output = git_mutation_output(root, args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_command_error(root, args, &output))
    }
}

fn git_mutation_output(root: &Path, args: &[&str]) -> Result<Output, DispatchError> {
    git_command(root, args).output().map_err(|error| {
        DispatchError::CliInvocationPermanent(format!(
            "mutate Git state in '{}' with `git {}`: {error}",
            root.display(),
            args.join(" ")
        ))
    })
}
