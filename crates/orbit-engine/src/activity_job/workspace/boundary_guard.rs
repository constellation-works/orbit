use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::declared_pair::{
    declared_workspace_path, exact_canonical_dir, git_top_level, task_id, worktree_mismatch_error,
};
use super::fingerprint::{
    GitPathState, GitWorktreeFingerprint, cached_primary_before_fingerprint, changed_paths,
    git_fingerprint, git_output_raw,
};
use super::{DeclaredWorktreePair, DispatchError, V2AuditWriter, WorktreeBoundaryGuard};

impl WorktreeBoundaryGuard {
    pub(crate) fn capture(
        input: &Value,
        task_ctx: Option<&Value>,
        run_id: &str,
        provider: &str,
        subprocess_cwd: Option<&Path>,
        registered_primary_root: Option<&Path>,
        declared_pair: Option<&DeclaredWorktreePair>,
    ) -> Result<Option<Self>, DispatchError> {
        let Some(pair) = declared_pair else {
            let (Some(subprocess_cwd), Some(registered_primary_root)) =
                (subprocess_cwd, registered_primary_root)
            else {
                return Ok(None);
            };
            let assigned_path =
                exact_canonical_dir(subprocess_cwd, "subprocess cwd").map_err(|reason| {
                    worktree_mismatch_error(
                        input,
                        task_ctx,
                        run_id,
                        provider,
                        declared_workspace_path(input, task_ctx).as_deref(),
                        None,
                        Some(subprocess_cwd),
                        Some(registered_primary_root),
                        reason,
                    )
                })?;
            let primary_path =
                exact_canonical_dir(registered_primary_root, "registered primary root").map_err(
                    |reason| {
                        worktree_mismatch_error(
                            input,
                            task_ctx,
                            run_id,
                            provider,
                            declared_workspace_path(input, task_ctx).as_deref(),
                            None,
                            Some(&assigned_path),
                            Some(registered_primary_root),
                            reason,
                        )
                    },
                )?;
            if assigned_path == primary_path {
                return Ok(None);
            }

            let assigned_git_root = git_top_level(&assigned_path).map_err(|error| {
                worktree_mismatch_error(
                    input,
                    task_ctx,
                    run_id,
                    provider,
                    declared_workspace_path(input, task_ctx).as_deref(),
                    None,
                    Some(&assigned_path),
                    Some(&primary_path),
                    format!("cannot resolve subprocess Git checkout: {error}"),
                )
            })?;
            let primary_git_root = git_top_level(&primary_path).map_err(|error| {
                worktree_mismatch_error(
                    input,
                    task_ctx,
                    run_id,
                    provider,
                    declared_workspace_path(input, task_ctx).as_deref(),
                    None,
                    Some(&assigned_path),
                    Some(&primary_path),
                    format!("cannot resolve registered primary Git checkout: {error}"),
                )
            })?;
            if assigned_git_root.is_some() && assigned_git_root == primary_git_root {
                return Ok(None);
            }
            return Err(worktree_mismatch_error(
                input,
                task_ctx,
                run_id,
                provider,
                declared_workspace_path(input, task_ctx).as_deref(),
                None,
                assigned_git_root.as_deref().or(Some(&assigned_path)),
                primary_git_root.as_deref().or(Some(&primary_path)),
                "distinct checkouts require a declared, matching workspace_path/repo_root pair"
                    .to_string(),
            ));
        };

        let subprocess_cwd = subprocess_cwd.ok_or_else(|| {
            worktree_mismatch_error(
                input,
                task_ctx,
                run_id,
                provider,
                Some(&pair.requested_workspace_path),
                Some(&pair.requested_repo_root),
                Some(&pair.assigned_root),
                Some(&pair.primary_root),
                "validated worktree pair did not resolve a subprocess cwd".to_string(),
            )
        })?;
        let resolved_cwd =
            exact_canonical_dir(subprocess_cwd, "subprocess cwd").map_err(|reason| {
                worktree_mismatch_error(
                    input,
                    task_ctx,
                    run_id,
                    provider,
                    Some(&pair.requested_workspace_path),
                    Some(&pair.requested_repo_root),
                    Some(&pair.assigned_root),
                    Some(&pair.primary_root),
                    reason,
                )
            })?;
        if resolved_cwd != pair.assigned_root {
            return Err(worktree_mismatch_error(
                input,
                task_ctx,
                run_id,
                provider,
                Some(&pair.requested_workspace_path),
                Some(&pair.requested_repo_root),
                Some(&resolved_cwd),
                Some(&pair.primary_root),
                "resolved subprocess cwd differs from the validated assigned checkout".to_string(),
            ));
        }
        let registered_primary_root = registered_primary_root.ok_or_else(|| {
            worktree_mismatch_error(
                input,
                task_ctx,
                run_id,
                provider,
                Some(&pair.requested_workspace_path),
                Some(&pair.requested_repo_root),
                Some(&pair.assigned_root),
                None,
                "registered primary checkout disappeared after pair validation".to_string(),
            )
        })?;
        let resolved_primary =
            exact_canonical_dir(registered_primary_root, "registered primary root").map_err(
                |reason| {
                    worktree_mismatch_error(
                        input,
                        task_ctx,
                        run_id,
                        provider,
                        Some(&pair.requested_workspace_path),
                        Some(&pair.requested_repo_root),
                        Some(&pair.assigned_root),
                        Some(registered_primary_root),
                        reason,
                    )
                },
            )?;
        if resolved_primary != pair.primary_root {
            return Err(worktree_mismatch_error(
                input,
                task_ctx,
                run_id,
                provider,
                Some(&pair.requested_workspace_path),
                Some(&pair.requested_repo_root),
                Some(&pair.assigned_root),
                Some(&resolved_primary),
                "registered primary checkout changed after pair validation".to_string(),
            ));
        }

        let assigned_root = pair.assigned_root.clone();
        let primary_root = pair.primary_root.clone();
        let requested_workspace_path = pair.requested_workspace_path.clone();
        let requested_repo_root = Some(pair.requested_repo_root.clone());
        let task_id = task_id(input, task_ctx);

        Ok(Some(Self {
            task_id,
            run_id: run_id.to_string(),
            provider: provider.to_string(),
            requested_workspace_path,
            requested_repo_root,
            assigned_before: git_fingerprint(&assigned_root)?,
            primary_before: cached_primary_before_fingerprint(run_id, &primary_root)?,
            assigned_root,
            primary_root,
            rebase_recovery: None,
            audit: None,
        }))
    }

    /// Bind the run's audit trail so an integrity violation can persist its
    /// full fingerprint evidence as a blob instead of inlining it into the
    /// error string every consumer then copies [ORB-12467].
    pub(crate) fn with_audit(mut self, audit: Arc<V2AuditWriter>) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Compare both monitored checkouts after the provider reaches any
    /// terminal outcome. Primary working-copy and index movement is observed
    /// but never attributed to the provider from before/after snapshots alone.
    /// A stationary primary HEAD is therefore benign regardless of path class
    /// or overlap with the candidate. A proven same-branch fast-forward is also
    /// benign: linked worktrees retain their own HEAD, and the later fetched-base
    /// rebase is the authority for candidate-versus-target integration. Primary
    /// rewrites, primary branch switches, and unapproved history changes in the
    /// assigned worktree remain typed, fail-closed violations. Only completion
    /// of an explicitly admitted stopped rebase permits assigned history changes.
    pub(crate) fn verify_after_provider(
        self,
        host: &dyn RuntimeHost,
        recovery_succeeded: bool,
        recovery_step_id: Option<&str>,
        task_ids: &[String],
    ) -> Result<(), DispatchError> {
        let completion = if self.rebase_recovery.is_some() && recovery_succeeded {
            let step_id = recovery_step_id.ok_or_else(|| {
                DispatchError::CliInvocationPermanent(
                    "conflict recovery is missing its failed step identity".to_string(),
                )
            })?;
            Some((
                step_id,
                self.complete_rebase_recovery(host, step_id, task_ids)?,
            ))
        } else {
            None
        };
        self.verify()?;
        if let Some((step_id, output)) = completion {
            host.checkpoint_rebase_recovery(&self.run_id, step_id, &output)?;
        }
        Ok(())
    }

    pub(crate) fn verify(&self) -> Result<(), DispatchError> {
        let assigned_after = git_fingerprint(&self.assigned_root)?;
        let primary_after = git_fingerprint(&self.primary_root)?;
        let assigned_history_changed = assigned_after.head != self.assigned_before.head
            || assigned_after.branch != self.assigned_before.branch;
        let run_changed_paths =
            changed_paths(&self.assigned_root, &self.assigned_before, &assigned_after);
        let primary_changed_paths =
            changed_paths(&self.primary_root, &self.primary_before, &primary_after);
        // Interference is judged against the primary's *dirt*, not against the
        // commits a fast-forward brought in: a merged sibling PR that touched
        // the same file the run touched is base advance, which the shipment
        // rebase checkpoint reconciles, not a boundary violation.
        let primary_dirt_paths = primary_dirt_mutations(&self.primary_before, &primary_after);
        let run_path_index = run_changed_paths.iter().collect::<BTreeSet<_>>();
        let conflicting_paths = primary_dirt_paths
            .iter()
            .filter(|path| run_path_index.contains(path))
            .cloned()
            .collect::<Vec<_>>();

        if assigned_history_changed && !self.completed_authorized_rebase(&assigned_after)? {
            // Only the checkpointed conflict-recovery leaf may finish a rebase.
            return Err(self.integrity_error(
                "worktree_content_conflict",
                &assigned_after,
                &primary_after,
                &run_changed_paths,
                &primary_changed_paths,
                &primary_dirt_paths,
                &conflicting_paths,
                "the provider changed the assigned worktree HEAD or branch; providers may edit \
                 files but must not create commits or move HEAD",
            ));
        }

        if primary_after == self.primary_before {
            return Ok(());
        }

        if primary_stationary_dirt_delta_is_benign(
            &self.primary_before,
            &primary_after,
            &primary_dirt_paths,
        ) {
            tracing::info!(
                target: "orbit.engine.cli_runner",
                task_id = %self.task_id,
                run_id = %self.run_id,
                primary_head = %primary_after.head,
                ignored_primary_paths = ?primary_dirt_paths,
                "accepted concurrent primary working-state movement; primary HEAD and branch never moved, and fetched-target integration remains authoritative"
            );
            return Ok(());
        }

        if primary_fast_forward_is_benign(&self.primary_root, &self.primary_before, &primary_after)?
        {
            tracing::info!(
                target: "orbit.engine.cli_runner",
                task_id = %self.task_id,
                run_id = %self.run_id,
                primary_before = %self.primary_before.head,
                primary_after = %primary_after.head,
                ignored_primary_paths = ?primary_dirt_paths,
                "accepted concurrent primary fast-forward; shipment base synchronization owns reconciliation"
            );
            return Ok(());
        }

        Err(self.integrity_error(
            "primary_checkout_drift",
            &assigned_after,
            &primary_after,
            &primary_changed_paths,
            &primary_changed_paths,
            &primary_dirt_paths,
            &conflicting_paths,
            "the registered primary checkout changed without a clean same-branch fast-forward",
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn integrity_error(
        &self,
        code: &'static str,
        assigned_after: &GitWorktreeFingerprint,
        primary_after: &GitWorktreeFingerprint,
        reported_paths: &[String],
        primary_changed_paths: &[String],
        primary_dirt_paths: &[String],
        conflicting_paths: &[String],
        reason: &str,
    ) -> DispatchError {
        let recovery = match self.preserve_dirty_assigned_worktree(assigned_after) {
            Ok(Some(artifact)) => json!(artifact),
            Ok(None) => Value::Null,
            Err(error) => json!({
                "preservation_error": error.to_string(),
            }),
        };
        // The four fingerprints are the bulk of this evidence and none of its
        // readability: every dirty path contributes up to four sha256 digests,
        // and an untracked tree contributes one more each. They go to the run's
        // audit blob store; the diagnostic keeps a summary and the blob's name.
        let fingerprints = json!({
            "assigned_before": self.assigned_before,
            "assigned_after": assigned_after,
            "primary_before": self.primary_before,
            "primary_after": primary_after,
        });
        let fingerprints_blob_ref = self
            .audit
            .as_ref()
            .map(|audit| audit.write_blob(fingerprints.to_string().as_bytes()));
        let diagnostic = json!({
            "code": code,
            "reason": reason,
            "task_id": self.task_id,
            "run_id": self.run_id,
            "provider": self.provider,
            "requested_workspace_path": self.requested_workspace_path,
            "requested_repo_root": self.requested_repo_root,
            "resolved_assigned_root": self.assigned_root,
            "registered_primary_root": self.primary_root,
            "changed_paths": reported_paths,
            "run_changed_paths": changed_paths(&self.assigned_root, &self.assigned_before, assigned_after),
            "primary_changed_paths": primary_changed_paths,
            "primary_dirt_paths": primary_dirt_paths,
            "conflicting_paths": conflicting_paths,
            "assigned_changed": assigned_after != &self.assigned_before,
            "assigned_before": fingerprint_summary(&self.assigned_before),
            "assigned_after": fingerprint_summary(assigned_after),
            "primary_before": fingerprint_summary(&self.primary_before),
            "primary_after": fingerprint_summary(primary_after),
            "fingerprints_blob_ref": fingerprints_blob_ref,
            "recovery": recovery,
            "automatic_reconciliation": false,
        });
        DispatchError::WorktreeIntegrity {
            code,
            diagnostic: diagnostic.to_string(),
        }
    }
}

/// The parts of a checkout fingerprint that identify it without enumerating it.
///
/// A full [`GitWorktreeFingerprint`] carries a `path_states` entry — up to four
/// sha256 digests — for every dirty path, plus an `untracked_content` digest per
/// untracked file. Four of those serialised into one error string reached 2.5 MB
/// on a 50-path `primary_checkout_drift`, and every consumer of that string
/// (`run.finished.error_message`, the worker log, the recovery agent's prompt)
/// then carried the copy. The digests stay retrievable from the audit blob named
/// by the diagnostic's `fingerprints_blob_ref` [ORB-12467].
fn fingerprint_summary(fingerprint: &GitWorktreeFingerprint) -> Value {
    json!({
        "head": fingerprint.head,
        "branch": fingerprint.branch,
        "index_sha256": fingerprint.index_sha256,
        "tracked_patch_sha256": fingerprint.tracked_patch_sha256,
        "dirty_paths": fingerprint.dirty_paths,
    })
}

/// Accept a primary checkout whose HEAD and branch stayed stationary while its
/// working copy or index changed.
///
/// `primary_fast_forward_is_benign` covers the case where the primary branch
/// advanced; it rejects `before.head == after.head` on its first clause, which
/// previously left source dirt reported as `primary_checkout_drift` after a
/// complete implementation.
///
/// The snapshots prove only that primary state moved during the invocation;
/// they cannot identify the writer. The primary is outside the candidate and
/// is never staged, committed, reset, or cleaned here. Candidate integration
/// is decided later in the assigned worktree against the fetched remote target,
/// where Git can distinguish a clean merge from a real conflict. The nonempty
/// dirt-path condition keeps unexplained fingerprint changes fail closed.
fn primary_stationary_dirt_delta_is_benign(
    before: &GitWorktreeFingerprint,
    after: &GitWorktreeFingerprint,
    primary_dirt_paths: &[String],
) -> bool {
    before.head == after.head && before.branch == after.branch && !primary_dirt_paths.is_empty()
}

fn primary_fast_forward_is_benign(
    root: &Path,
    before: &GitWorktreeFingerprint,
    after: &GitWorktreeFingerprint,
) -> Result<bool, DispatchError> {
    // Linked-worktree shipment owns clean base fast-forwards; the provider
    // boundary still rejects primary rewrites. Primary dirt and pathname
    // overlap do not establish candidate-versus-target interference; the later
    // fetched-base rebase owns that decision.
    if before.head == after.head || before.branch != after.branch {
        return Ok(false);
    }
    Ok(git_output_raw(
        root,
        &["merge-base", "--is-ancestor", &before.head, &after.head],
    )?
    .status
    .success())
}

/// Paths whose working-state identity in a checkout actually changed, judged
/// independently of HEAD movement.
///
/// `staged_patch_sha256` is deliberately excluded: it is a diff against HEAD,
/// so a concurrent fast-forward alone rewrites it for every already-dirty path
/// even though nobody touched the file. `index_entry_sha256` carries the same
/// staged content identity without that dependency.
fn primary_dirt_mutations(
    before: &GitWorktreeFingerprint,
    after: &GitWorktreeFingerprint,
) -> Vec<String> {
    fn dirt_identity(
        state: &GitPathState,
    ) -> (&Option<String>, &Option<String>, bool, &Option<String>) {
        (
            &state.index_entry_sha256,
            &state.worktree_patch_sha256,
            state.worktree_present,
            &state.untracked_content_sha256,
        )
    }

    before
        .path_states
        .keys()
        .chain(after.path_states.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| {
            before.path_states.get(*path).map(dirt_identity)
                != after.path_states.get(*path).map(dirt_identity)
        })
        .cloned()
        .collect()
}
