use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::context::{RebaseRecoveryAttemptScope, RuntimeHost};
use crate::executor::automation::vcs::absorbed::task_scope_digest;
use crate::executor::automation::vcs::recovery_published_head;

use super::super::fingerprint::{GitWorktreeFingerprint, git_output_raw, git_stdout};
use super::super::{DispatchError, WorktreeBoundaryGuard};
use super::checkpoint::RebaseRecoveryCheckpoint;
use super::git::{
    completion_recovery_base_ref, recovery_conflicting_paths, stopped_head_on_pin, unmerged_paths,
    validate_failed_step_identity,
};
use super::metadata::RecoveryMetadata;

impl WorktreeBoundaryGuard {
    /// Admit file repair for the already stopped, checkpoint-matching rebase.
    /// The provider remains unable to write Git metadata; after it exits, the
    /// host boundary independently validates and completes this checkpoint.
    /// Admission reserves the host's attempt identity for this recovery, so a
    /// later legitimate recovery of the same step certifies as a new attempt
    /// instead of colliding with this one.
    pub(crate) fn authorize_rebase_completion(
        &mut self,
        host: &dyn RuntimeHost,
        input: &Value,
    ) -> Result<(), DispatchError> {
        let invalid = |reason: &str| {
            DispatchError::CliInvocationPermanent(format!(
                "conflict recovery requires an existing rebase matching the prepared branch, \
                 original HEAD and pinned target base: {reason}"
            ))
        };
        if input.get("recovery_kind").and_then(Value::as_str) != Some("vcs_conflict")
            || input.get("operation").and_then(Value::as_str) != Some("git_rebase")
        {
            return Err(invalid(
                "recovery input does not name a git_rebase vcs_conflict",
            ));
        }
        let prepared = input
            .get("failed_step_input")
            .ok_or_else(|| invalid("missing failed_step_input"))?;
        let branch = prepared
            .get("head")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("missing prepared branch head"))?;
        let original = prepared
            .get("head_sha")
            .or_else(|| prepared.get("published_head_sha"))
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("missing prepared head_sha or published_head_sha"))?;
        let original_base_sha = input
            .get("original_base_sha")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("missing original_base_sha"))?;
        let target = input
            .get("target_base_sha")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid("missing target_base_sha"))?;
        let base_ref = prepared
            .get("base_ref")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| completion_recovery_base_ref(prepared))
            .ok_or_else(|| invalid("missing prepared base_ref"))?;
        let prepared_target = prepared
            .get("base_sha")
            .and_then(Value::as_str)
            .unwrap_or(target);
        let step_id = validate_failed_step_identity(input, prepared, original).map_err(invalid)?;
        // Completion's rendered input retains the initial publication even
        // after its watcher publishes a check repair. Authenticate that input
        // first, then bind Git's original head (and the publication lease) to
        // the contiguous repair heads the host recorded for this PR.
        // `authenticated_published_head_sha` is only the provider's pin;
        // reading it here would let a rendered input widen this boundary.
        let published_original = original;
        let original = if input.get("activity_name").and_then(Value::as_str) == Some("pr_complete")
        {
            let primary_task = prepared
                .get("completed_task_ids")
                .or_else(|| input.get("task_ids"))
                .and_then(Value::as_array)
                .and_then(|ids| ids.iter().filter_map(Value::as_str).min())
                .or_else(|| self.recovery_task_id(input))
                .ok_or_else(|| invalid("missing completion primary task"))?;
            recovery_published_head(
                host,
                &self.run_id,
                primary_task,
                prepared["pr_number"]
                    .as_str()
                    .ok_or_else(|| invalid("missing prepared pr_number"))?,
                original,
            )
            .map_err(|error| invalid(&format!("published head lookup failed: {error}")))?
        } else {
            original.to_string()
        };
        let original = original.as_str();
        // The stopped rebase is pinned by its own `onto` metadata below, not by
        // the moving base ref: a linked worktree shares remote-tracking refs
        // with every sibling checkout, so `origin/<base>` routinely advances
        // past the pin while the leaf runs. That advance is reconciled after
        // continuation, not refused here. The ref must still resolve so the
        // continuation can read its tip.
        git_stdout(&self.assigned_root, &["rev-parse", "--verify", &base_ref])?;
        if prepared_target != target {
            return Err(invalid("prepared base_sha does not match target_base_sha"));
        }
        let merge_base = git_stdout(&self.assigned_root, &["merge-base", original, target])?;
        if merge_base != original_base_sha {
            return Err(invalid(&format!(
                "merge-base '{merge_base}' of original HEAD '{original}' and target base \
                 '{target}' does not match original_base_sha '{original_base_sha}'"
            )));
        }
        let conflicting_paths = recovery_conflicting_paths(input).map_err(invalid)?;
        if unmerged_paths(&self.assigned_root)? != conflicting_paths {
            return Err(invalid(
                "worktree unmerged paths do not match conflicting_paths",
            ));
        }
        let expected_ref = format!("refs/heads/{branch}");
        let branch_head = git_stdout(
            &self.assigned_root,
            &["rev-parse", "--verify", &expected_ref],
        )?;
        if branch_head != original {
            return Err(invalid(&format!(
                "branch '{expected_ref}' is at '{branch_head}', expected original HEAD '{original}'"
            )));
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
                fs::read_to_string(path.join(name))
                    .map(|text| text.trim().to_string())
                    .map_err(|_| invalid(&format!("missing rebase metadata '{name}'")))
            };
            let head_name = read("head-name")?;
            if head_name != expected_ref {
                return Err(invalid(&format!(
                    "rebase head-name '{head_name}' does not match '{expected_ref}'"
                )));
            }
            let orig_head = read("orig-head")?;
            if orig_head != original {
                return Err(invalid(&format!(
                    "rebase orig-head '{orig_head}' does not match original HEAD '{original}'"
                )));
            }
            let onto = read("onto")?;
            if onto != target {
                return Err(invalid(&format!(
                    "rebase onto '{onto}' does not match target base '{target}'"
                )));
            }
            if !stopped_head_on_pin(&self.assigned_root, target)? {
                return Err(invalid(
                    "stopped rebase HEAD does not descend from target base",
                ));
            }
            let metadata = RecoveryMetadata::capture(&self.assigned_root, path)?;
            let task_scope_digest = self
                .recovery_task_id(input)
                .and_then(|task_id| host.get_task(task_id).ok())
                .map(|task| task_scope_digest(&task));
            let attempt = host.begin_rebase_recovery_attempt(
                &self.run_id,
                step_id,
                &RebaseRecoveryAttemptScope {
                    workspace_path: self.assigned_root.to_string_lossy().into_owned(),
                    head_sha_before: original.to_string(),
                    target_base_sha: target.to_string(),
                },
            )?;
            self.rebase_recovery = Some(RebaseRecoveryCheckpoint {
                metadata,
                step_id: step_id.to_string(),
                attempt,
                branch: branch.to_string(),
                original_head: original.to_string(),
                original_base_sha: original_base_sha.to_string(),
                base_ref,
                target_base_sha: target.to_string(),
                conflicting_paths,
                remote_sha_before: if original != published_original {
                    Some(original.to_string())
                } else {
                    prepared
                        .get("remote_sha")
                        .or_else(|| prepared.get("published_head_sha"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                },
                task_scope_digest,
            });
            return Ok(());
        }
        Err(invalid(
            "no rebase-merge or rebase-apply directory in worktree",
        ))
    }

    /// The task this recovery serves: the guard's own, else the recovery
    /// input's single task.
    fn recovery_task_id<'a>(&'a self, input: &'a Value) -> Option<&'a str> {
        if self.task_id != "unknown" {
            return Some(self.task_id.as_str());
        }
        match input.get("task_ids").and_then(Value::as_array) {
            Some(ids) if ids.len() == 1 => ids[0].as_str(),
            Some(_) => None,
            None => input.get("task_id").and_then(Value::as_str),
        }
    }

    /// The admitted rebase's checkpoint, when `after` shows it finished on
    /// its branch with no rebase state left.
    pub(super) fn finished_authorized_rebase(
        &self,
        after: &GitWorktreeFingerprint,
    ) -> Result<Option<&RebaseRecoveryCheckpoint>, DispatchError> {
        let Some(checkpoint) = &self.rebase_recovery else {
            return Ok(None);
        };
        if after.branch.as_deref() != Some(checkpoint.branch.as_str()) {
            return Ok(None);
        }
        for backend in ["rebase-merge", "rebase-apply"] {
            let path = git_stdout(
                &self.assigned_root,
                &["rev-parse", "--path-format=absolute", "--git-path", backend],
            )?;
            if Path::new(&path).exists() {
                return Ok(None);
            }
        }
        Ok(Some(checkpoint))
    }

    /// Whether the admitted rebase finished exactly on its pinned base, with
    /// no candidate commit left: the shape an absorbed candidate leaves.
    pub(in crate::activity_job::workspace) fn absorbed_authorized_rebase(
        &self,
        after: &GitWorktreeFingerprint,
    ) -> Result<bool, DispatchError> {
        Ok(self
            .finished_authorized_rebase(after)?
            .is_some_and(|checkpoint| after.head == checkpoint.target_base_sha))
    }

    pub(in crate::activity_job::workspace) fn completed_authorized_rebase(
        &self,
        after: &GitWorktreeFingerprint,
    ) -> Result<bool, DispatchError> {
        let Some(checkpoint) = self.finished_authorized_rebase(after)? else {
            return Ok(false);
        };
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
            .success)
    }

    /// Whether the assigned worktree is still inside this recovery's pinned
    /// rebase, stopped on unmerged paths: the shape a host continuation that
    /// stopped on a later commit leaves behind.
    pub(in crate::activity_job::workspace) fn stopped_authorized_rebase(
        &self,
    ) -> Result<bool, DispatchError> {
        let Some(checkpoint) = &self.rebase_recovery else {
            return Ok(false);
        };
        let invalid = |reason: &str| DispatchError::CliInvocationPermanent(reason.to_string());
        Ok(self
            .validate_rebase_checkpoint(checkpoint, &invalid)
            .is_ok()
            && !unmerged_paths(&self.assigned_root)?.is_empty())
    }
}
