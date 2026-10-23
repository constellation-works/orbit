use std::collections::BTreeMap;

use serde_json::Value;

use orbit_types::task::ContextWideningStep;

use crate::context::RuntimeHost;
use crate::executor::automation::vcs::absorbed::{ABSORBING_STEP, AbsorbedReason, covering_commit};

use super::super::boundary_guard::is_host_owned_path;
use super::super::fingerprint::{
    GitWorktreeFingerprint, changed_paths, git_fingerprint, git_output_raw, git_stdout,
};
use super::super::generated_paths::{conflicted_generated_paths, regenerate};
use super::super::rebase_repairs::{earlier_companion_paths, unrepaired_conflict_paths};
use super::super::{DispatchError, WorktreeBoundaryGuard};
use super::checkpoint::{RebaseContinuation, RebaseRecoveryCheckpoint};
use super::git::{
    git_mutation, git_mutation_output, index_unchanged, recheck_recovery_owner, unmerged_paths,
};

impl WorktreeBoundaryGuard {
    pub(in crate::activity_job::workspace) fn complete_rebase_recovery(
        &self,
        host: &dyn RuntimeHost,
        step_id: &str,
        task_ids: &[String],
    ) -> Result<RebaseContinuation, DispatchError> {
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
        if step_id != checkpoint.step_id {
            return Err(invalid(
                "the failed step differs from the admitted recovery",
            ));
        }

        // Authenticate metadata before snapshotting or staging repaired files.
        // Matching commits/index alone cannot authenticate a copy.
        checkpoint.metadata.verify(&self.assigned_root)?;
        let after_agent = git_fingerprint(&self.assigned_root)?;
        if after_agent.head != self.assigned_before.head
            || after_agent.branch != self.assigned_before.branch
            || !index_unchanged(&self.assigned_before, &after_agent)
        {
            return Err(invalid(
                "the provider changed HEAD, branch, or index instead of only repairing files",
            ));
        }
        // Generated conflict files are rebuilt, not merged: take a side, run
        // the configured command, and stage its output with the resolution.
        // The command must leave Git metadata, HEAD, branch and index as the
        // provider had to [ORB-16784].
        let regenerated = conflicted_generated_paths(host, &checkpoint.conflicting_paths);
        let after_agent = if regenerated.is_empty() {
            after_agent
        } else {
            // The command runs in the checkout: only while this recovery
            // still owns it, as staging is.
            self.validate_rebase_checkpoint(checkpoint, &invalid)?;
            recheck_recovery_owner(
                host,
                &self.run_id,
                step_id,
                task_ids,
                &self.assigned_root,
                &invalid,
            )?;
            regenerate(host, &self.assigned_root, &regenerated).map_err(|reason| {
                invalid(&format!(
                    "regenerating a generated conflict path failed: {reason}"
                ))
            })?;
            checkpoint.metadata.verify(&self.assigned_root)?;
            let regenerated_state = git_fingerprint(&self.assigned_root)?;
            if regenerated_state.head != self.assigned_before.head
                || regenerated_state.branch != self.assigned_before.branch
                || !index_unchanged(&self.assigned_before, &regenerated_state)
            {
                return Err(invalid(
                    "a generated-path command changed HEAD, branch, or index instead of only writing files",
                ));
            }
            regenerated_state
        };
        let regenerated_paths = regenerated
            .iter()
            .map(|generated| generated.path.clone())
            .collect::<Vec<_>>();
        let changed = changed_paths(&self.assigned_root, &self.assigned_before, &after_agent);
        // A conflict file counts as repaired when this invocation changed it,
        // or when an earlier actor (final recovery) already did [F2026-10-237].
        // A regenerated file is repaired by its command.
        let hand_repaired = checkpoint
            .conflicting_paths
            .iter()
            .filter(|path| !regenerated_paths.contains(path))
            .cloned()
            .collect::<Vec<_>>();
        if !unrepaired_conflict_paths(&self.assigned_root, &hand_repaired, &changed)?.is_empty() {
            return Err(invalid(
                "one or more authorized conflict files were not repaired",
            ));
        }
        // A resolution may need companion edits beyond the conflict set (a
        // re-export for a moved module, a caller of a renamed item). Every
        // other path the provider changed joins the continued commit, as do
        // unstaged tracked repairs made before it; only host-owned `.orbit/`
        // state, a pre-existing untracked file the provider removed, and
        // untracked payloads that predate the invocation stay out of it.
        let mut companion_paths =
            earlier_companion_paths(&self.assigned_before, &checkpoint.conflicting_paths);
        companion_paths.extend(
            changed
                .iter()
                .filter(|path| !checkpoint.conflicting_paths.contains(path))
                .filter(|path| !is_host_owned_path(path))
                .filter(|path| {
                    after_agent.path_states.get(*path).is_none_or(|state| {
                        state.worktree_present || state.index_entry_sha256.is_some()
                    })
                })
                .cloned(),
        );
        let companion_paths = companion_paths.into_iter().collect::<Vec<_>>();
        self.validate_rebase_checkpoint(checkpoint, &invalid)?;
        if unmerged_paths(&self.assigned_root)? != checkpoint.conflicting_paths {
            return Err(invalid(
                "the live unmerged paths changed after provider execution",
            ));
        }

        // Regenerated files hold their command's output, already checked for
        // markers; whitespace there is the generator's, not a bad merge.
        let mut diff_check_args = vec!["diff", "--check", "--"];
        diff_check_args.extend(hand_repaired.iter().map(String::as_str));
        if !hand_repaired.is_empty()
            && !git_output_raw(&self.assigned_root, &diff_check_args)?.success
        {
            return Err(invalid(
                "a repaired file still contains conflict markers or whitespace errors",
            ));
        }

        recheck_recovery_owner(
            host,
            &self.run_id,
            step_id,
            task_ids,
            &self.assigned_root,
            &invalid,
        )?;

        let mut add_args = vec!["add", "--all", "--"];
        add_args.extend(checkpoint.conflicting_paths.iter().map(String::as_str));
        add_args.extend(companion_paths.iter().map(String::as_str));
        git_mutation(&self.assigned_root, &add_args)?;
        if !unmerged_paths(&self.assigned_root)?.is_empty() {
            return Err(invalid(
                "staging the resolved conflict set left unresolved entries",
            ));
        }
        // Untracked files the continuation commits are no longer untracked;
        // every other pre-existing untracked payload must survive unchanged.
        let mut expected_untracked = self.assigned_before.untracked_content.clone();
        expected_untracked.retain(|path, _| !companion_paths.contains(path));
        let continued = git_mutation_output(
            &self.assigned_root,
            &["-c", "core.editor=true", "rebase", "--continue"],
        )?;
        if !continued.success {
            let additional = unmerged_paths(&self.assigned_root)?;
            // A candidate with several commits can conflict again on a later
            // pick. The repaired pick is committed; keep that progress for
            // the next bounded recovery round instead of refusing it.
            if !additional.is_empty() && self.stopped_authorized_rebase()? {
                self.widen_task(
                    host,
                    task_ids,
                    ContextWideningStep::Recovery,
                    &companion_paths,
                );
                tracing::warn!(
                    target: "orbit.engine.cli_runner",
                    run_id = %self.run_id,
                    step_id,
                    pinned_base = %checkpoint.target_base_sha,
                    additional_conflicting_paths = ?additional,
                    "conflict recovery continued the rebase onto a later commit that conflicts again"
                );
                return Ok(RebaseContinuation::StoppedAgain);
            }
            let diagnostic = continued.stderr.trim().to_string();
            return Err(invalid(&format!(
                "rebase --continue failed; additional_conflicting_paths={additional:?}; diagnostic={diagnostic}"
            )));
        }

        let completed = git_fingerprint(&self.assigned_root)?;
        if self.absorbed_authorized_rebase(&completed)? {
            self.require_clean_continuation(&completed, &expected_untracked, &invalid)?;
            return self
                .absorbed_continuation(
                    checkpoint,
                    step_id,
                    task_ids,
                    &completed,
                    &checkpoint.target_base_sha,
                    &invalid,
                )
                .map(RebaseContinuation::Absorbed);
        }
        if !self.completed_authorized_rebase(&completed)? {
            return Err(invalid(
                "continued rebase did not leave the checkpointed branch on the pinned base with a candidate commit",
            ));
        }
        self.require_clean_continuation(&completed, &expected_untracked, &invalid)?;
        // The pin only records where the rebase stopped. Land the continued
        // candidate on the base tip that is live now, so the deterministic
        // retry and the PR see current integration rather than a stale one.
        let landed = self.follow_advanced_base(checkpoint, &expected_untracked, &invalid)?;
        let completed = git_fingerprint(&self.assigned_root)?;
        let base_sha = match landed {
            LandedOn::Candidate(base_sha) => base_sha,
            LandedOn::Absorbed(base_sha) => {
                return self
                    .absorbed_continuation(
                        checkpoint, step_id, task_ids, &completed, &base_sha, &invalid,
                    )
                    .map(RebaseContinuation::Absorbed);
            }
        };
        self.widen_task(
            host,
            task_ids,
            ContextWideningStep::Recovery,
            &companion_paths,
        );
        Ok(RebaseContinuation::Completed(serde_json::json!({
            "run_id": self.run_id,
            "step_id": step_id,
            "task_ids": task_ids,
            "workspace_path": self.assigned_root,
            "head": checkpoint.branch,
            "head_sha_before": checkpoint.original_head,
            "original_base_sha": checkpoint.original_base_sha,
            "base_ref": checkpoint.base_ref,
            "target_base_sha": checkpoint.target_base_sha,
            "base_sha": base_sha,
            "remote_sha_before": checkpoint.remote_sha_before,
            "head_sha": completed.head,
            "companion_paths": companion_paths,
            "regenerated_paths": regenerated_paths,
            "rewritten": true,
            "recovery_attempt": checkpoint.attempt,
        })))
    }

    /// The certified evidence for a continuation that left the branch on
    /// `landed_base` (the pin, or the advanced tip it followed) with no
    /// candidate commit: the covering commit, the candidate paths it covers
    /// and the task scope admitted with the recovery. Without all three the
    /// continuation is refused as before, naming the typed reason.
    fn absorbed_continuation(
        &self,
        checkpoint: &RebaseRecoveryCheckpoint,
        step_id: &str,
        task_ids: &[String],
        completed: &GitWorktreeFingerprint,
        landed_base: &str,
        invalid: &impl Fn(&str) -> DispatchError,
    ) -> Result<Value, DispatchError> {
        let refused = |reason: AbsorbedReason| {
            invalid(&format!(
                "continued rebase left the checkpointed branch on its base with no candidate \
                 commit; absorbed_candidate_refused: {}",
                reason.code()
            ))
        };
        if step_id != ABSORBING_STEP {
            return Err(invalid(
                "continued rebase did not leave the checkpointed branch on the pinned base with a candidate commit",
            ));
        }
        let task_scope_digest = checkpoint
            .task_scope_digest
            .clone()
            .ok_or_else(|| refused(AbsorbedReason::TaskScopeChanged))?;
        let (covering, candidate_paths) = covering_commit(
            &self.assigned_root,
            &checkpoint.original_base_sha,
            &checkpoint.original_head,
            landed_base,
        )
        .map_err(|error| invalid(&format!("find the covering commit: {error}")))?
        .ok_or_else(|| refused(AbsorbedReason::NoCoveringCommit))?;
        tracing::info!(
            target: "orbit.engine.cli_runner",
            run_id = %self.run_id,
            step_id,
            pinned_base = %checkpoint.target_base_sha,
            landed_base,
            covering_commit = %covering,
            "conflict recovery left no candidate commit; its base already covers the candidate"
        );
        Ok(serde_json::json!({
            "run_id": self.run_id,
            "step_id": step_id,
            "task_ids": task_ids,
            "workspace_path": self.assigned_root,
            "head": checkpoint.branch,
            "head_sha_before": checkpoint.original_head,
            "original_base_sha": checkpoint.original_base_sha,
            "base_ref": checkpoint.base_ref,
            "target_base_sha": checkpoint.target_base_sha,
            "base_sha": landed_base,
            "remote_sha_before": checkpoint.remote_sha_before,
            "head_sha": completed.head,
            "companion_paths": [],
            "rewritten": true,
            "recovery_attempt": checkpoint.attempt,
            "absorbed": {
                "covering_commit": covering,
                "candidate_paths": candidate_paths,
                "task_scope_digest": task_scope_digest,
            },
        }))
    }

    /// The stopped index includes every nonconflicting candidate change.
    /// Those staged paths become clean when Git commits them; comparing dirty
    /// path maps across that transition incorrectly rejects the candidate.
    /// Provider edits were checked before continuation. Now require Git to
    /// leave no tracked dirt and preserve the pre-existing untracked payload
    /// the continuation did not commit (`expected_untracked`).
    fn require_clean_continuation(
        &self,
        completed: &GitWorktreeFingerprint,
        expected_untracked: &BTreeMap<String, String>,
        invalid: &impl Fn(&str) -> DispatchError,
    ) -> Result<(), DispatchError> {
        if !git_output_raw(&self.assigned_root, &["diff", "--quiet", "HEAD", "--"])?.success
            || &completed.untracked_content != expected_untracked
        {
            return Err(invalid(
                "host continuation left tracked dirt or changed untracked files",
            ));
        }
        Ok(())
    }

    /// Rebase the continued candidate onto `base_ref`'s current tip when it
    /// advanced past the pin, returning the base the candidate now sits on.
    /// A follow-up that drops every pick as empty leaves the branch on the
    /// tip itself: [`LandedOn::Absorbed`].
    ///
    /// A tip that does not descend from the pin (a rewound or force-moved
    /// base) is not followed; the pinned result stands and the deterministic
    /// retry judges it. A follow-up that conflicts is aborted so the branch
    /// keeps the already-resolved pinned result: the retry then reports the
    /// remaining advance instead of a lost resolution.
    fn follow_advanced_base(
        &self,
        checkpoint: &RebaseRecoveryCheckpoint,
        expected_untracked: &BTreeMap<String, String>,
        invalid: &impl Fn(&str) -> DispatchError,
    ) -> Result<LandedOn, DispatchError> {
        let pinned = checkpoint.target_base_sha.as_str();
        let live = git_stdout(
            &self.assigned_root,
            &["rev-parse", "--verify", &checkpoint.base_ref],
        )?;
        if live == pinned
            || !git_output_raw(
                &self.assigned_root,
                &["merge-base", "--is-ancestor", pinned, &live],
            )?
            .success
        {
            return Ok(LandedOn::Candidate(pinned.to_string()));
        }
        let followed = git_mutation_output(&self.assigned_root, &["rebase", &live])?;
        if !followed.success {
            let additional = unmerged_paths(&self.assigned_root)?;
            git_mutation(&self.assigned_root, &["rebase", "--abort"])?;
            let restored = git_fingerprint(&self.assigned_root)?;
            if !self.completed_authorized_rebase(&restored)? {
                return Err(invalid(
                    "aborting the follow-up rebase onto the advanced base did not restore the pinned result",
                ));
            }
            self.require_clean_continuation(&restored, expected_untracked, invalid)?;
            tracing::warn!(
                target: "orbit.engine.cli_runner",
                run_id = %self.run_id,
                pinned_base = pinned,
                advanced_base = %live,
                additional_conflicting_paths = ?additional,
                "conflict recovery kept the pinned result; the advanced base conflicts again"
            );
            return Ok(LandedOn::Candidate(pinned.to_string()));
        }
        let followed = git_fingerprint(&self.assigned_root)?;
        if followed.head == live && self.finished_authorized_rebase(&followed)?.is_some() {
            self.require_clean_continuation(&followed, expected_untracked, invalid)?;
            return Ok(LandedOn::Absorbed(live));
        }
        if !self.completed_authorized_rebase(&followed)?
            || followed.head == live
            || !git_output_raw(
                &self.assigned_root,
                &["merge-base", "--is-ancestor", &live, "HEAD"],
            )?
            .success
        {
            return Err(invalid(
                "rebasing onto the advanced base did not leave the checkpointed branch on that base with a candidate commit",
            ));
        }
        self.require_clean_continuation(&followed, expected_untracked, invalid)?;
        Ok(LandedOn::Candidate(live))
    }
}

/// Where [`WorktreeBoundaryGuard::follow_advanced_base`] left the branch.
enum LandedOn {
    /// A candidate commit on this base.
    Candidate(String),
    /// Exactly this base: the follow-up dropped every pick as empty.
    Absorbed(String),
}
