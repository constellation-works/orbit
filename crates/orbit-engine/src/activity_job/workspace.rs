use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::dispatcher::DispatchError;

pub(crate) mod fingerprint;
mod rebase_recovery;

use fingerprint::{
    GitPathState, GitWorktreeFingerprint, changed_paths, git_fingerprint, git_output_raw,
    git_stdout_bytes,
};
use rebase_recovery::RebaseRecoveryCheckpoint;

pub fn resolve_subprocess_cwd(
    input: &Value,
    task_ctx: Option<&Value>,
    tool_ctx_workspace_root: Option<&Path>,
) -> Result<Option<PathBuf>, DispatchError> {
    // A *declared* workspace_path must be usable. Fail closed if the key is
    // present but renders to a non-string, null, or empty value — a
    // worktree-based pipeline step whose `{{ ... workspace_path }}` failed to
    // render would otherwise fall through and silently run the agent in the
    // primary checkout, which is the ORB-10134 data-loss hazard. A genuinely
    // absent key (direct, non-worktree runs) still falls back to the tool
    // context's workspace_root below.
    if let Some(resolved) = resolve_declared_workspace_path(Some(input), "activity input")? {
        return Ok(Some(resolved));
    }
    if let Some(resolved) = resolve_declared_workspace_path(task_ctx, "task context")? {
        return Ok(Some(resolved));
    }

    let Some(path) = tool_ctx_workspace_root else {
        return Ok(None);
    };

    if path.is_dir() {
        return Ok(Some(canonicalize_dir(path)));
    }

    tracing::warn!(
        target: "orbit.engine.cli_runner",
        path = %path.display(),
        "tool_ctx workspace_root missing, child will inherit parent cwd"
    );
    Ok(None)
}

/// Resolve a `workspace_path` declared on `container`. Returns:
/// - `Ok(None)` when the key is absent or JSON `null` (caller falls back — the
///   agent envelope / task context always serialize an undeclared
///   workspace_path as `null`, so `null` means "not declared");
/// - `Ok(Some(dir))` when it is a valid, existing directory;
/// - `Err(..)` when it is present but a non-string/non-null value or empty, or
///   names a path that is not a writable directory (fail closed — never fall
///   back to the primary checkout).
fn resolve_declared_workspace_path(
    container: Option<&Value>,
    source: &str,
) -> Result<Option<PathBuf>, DispatchError> {
    let Some(value) = container.and_then(|container| container.get("workspace_path")) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(path) = value.as_str() else {
        return Err(DispatchError::CliInvocationFailed(format!(
            "{source} declared a non-string workspace_path ({value}); \
             refusing to fall back to the repository root"
        )));
    };
    validate_declared_workspace_path(path).map(Some)
}

fn validate_declared_workspace_path(path: &str) -> Result<PathBuf, DispatchError> {
    let path_buf = PathBuf::from(path);
    if path.trim().is_empty() || !path_buf.is_dir() {
        return Err(DispatchError::CliInvocationFailed(format!(
            "workspace path {} is not a writable directory",
            path_buf.display()
        )));
    }
    Ok(canonicalize_dir(&path_buf))
}

// pub(crate) widened for tests/ layout under ORB-00225; test reaches via exposed surface.
pub(crate) fn canonicalize_dir(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// A pipeline-declared linked-worktree assignment that has been validated
/// against the runtime's registered primary checkout before provider setup.
#[derive(Debug, Clone)]
pub(crate) struct DeclaredWorktreePair {
    requested_workspace_path: String,
    requested_repo_root: String,
    assigned_root: PathBuf,
    primary_root: PathBuf,
}

/// Validate the two independently rendered path fields used by task shipment.
///
/// `workspace_path` controls the child cwd while `repo_root` is included in the
/// agent contract. Treating either one as advisory lets a partially rendered
/// pipeline place the provider in one checkout while telling it to edit
/// another. A declared pair therefore has to name the same exact linked
/// worktree, and that worktree has to be a non-primary checkout of the
/// registered repository. Every failure is the same typed, non-retryable
/// boundary error used for a post-spawn escape.
pub(crate) fn validate_declared_worktree_pair(
    input: &Value,
    task_ctx: Option<&Value>,
    run_id: &str,
    provider: &str,
    registered_primary_root: Option<&Path>,
) -> Result<Option<DeclaredWorktreePair>, DispatchError> {
    let Some(repo_root_value) = input.get("repo_root") else {
        return Ok(None);
    };

    let workspace_path_value = input
        .get("workspace_path")
        .filter(|value| !value.is_null())
        .or_else(|| {
            task_ctx
                .and_then(|task| task.get("workspace_path"))
                .filter(|value| !value.is_null())
        });
    let requested_workspace_path = workspace_path_value.map(declared_value_text);
    let requested_repo_root = declared_value_text(repo_root_value);
    let mismatch = |reason: String, assigned_root: Option<&Path>, primary_root: Option<&Path>| {
        worktree_mismatch_error(
            input,
            task_ctx,
            run_id,
            provider,
            requested_workspace_path.as_deref(),
            Some(&requested_repo_root),
            assigned_root,
            primary_root,
            reason,
        )
    };

    let workspace_path = workspace_path_value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| {
            mismatch(
                "declared repo_root requires a non-empty string workspace_path".to_string(),
                None,
                registered_primary_root,
            )
        })?;
    let repo_root = repo_root_value
        .as_str()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| {
            mismatch(
                "declared repo_root must be a non-empty string".to_string(),
                None,
                registered_primary_root,
            )
        })?;

    let workspace_path = exact_canonical_dir(Path::new(workspace_path), "workspace_path")
        .map_err(|reason| mismatch(reason, None, registered_primary_root))?;
    let repo_root = exact_canonical_dir(Path::new(repo_root), "repo_root")
        .map_err(|reason| mismatch(reason, Some(&workspace_path), registered_primary_root))?;
    let assigned_root = required_git_top_level(&workspace_path, "workspace_path")
        .map_err(|reason| mismatch(reason, Some(&workspace_path), registered_primary_root))?;
    let repo_git_root = required_git_top_level(&repo_root, "repo_root")
        .map_err(|reason| mismatch(reason, Some(&assigned_root), registered_primary_root))?;

    if workspace_path != assigned_root {
        return Err(mismatch(
            format!(
                "workspace_path must name the exact Git checkout root '{}', not '{}'",
                assigned_root.display(),
                workspace_path.display()
            ),
            Some(&assigned_root),
            registered_primary_root,
        ));
    }
    if repo_root != repo_git_root {
        return Err(mismatch(
            format!(
                "repo_root must name the exact Git checkout root '{}', not '{}'",
                repo_git_root.display(),
                repo_root.display()
            ),
            Some(&assigned_root),
            registered_primary_root,
        ));
    }
    if assigned_root != repo_git_root {
        return Err(mismatch(
            "workspace_path and repo_root resolve to different Git checkouts".to_string(),
            Some(&assigned_root),
            registered_primary_root,
        ));
    }

    let primary_path = registered_primary_root.ok_or_else(|| {
        mismatch(
            "declared worktree pair requires a registered primary checkout".to_string(),
            Some(&assigned_root),
            None,
        )
    })?;
    let primary_path = exact_canonical_dir(primary_path, "registered primary root")
        .map_err(|reason| mismatch(reason, Some(&assigned_root), Some(primary_path)))?;
    let primary_root = required_git_top_level(&primary_path, "registered primary root")
        .map_err(|reason| mismatch(reason, Some(&assigned_root), Some(&primary_path)))?;
    if primary_path != primary_root {
        return Err(mismatch(
            format!(
                "registered primary root must name the exact Git checkout root '{}', not '{}'",
                primary_root.display(),
                primary_path.display()
            ),
            Some(&assigned_root),
            Some(&primary_root),
        ));
    }
    if assigned_root == primary_root {
        return Err(mismatch(
            "assigned checkout collapses to the registered primary checkout".to_string(),
            Some(&assigned_root),
            Some(&primary_root),
        ));
    }

    let assigned_common_dir = required_git_common_dir(&assigned_root, "assigned checkout")
        .map_err(|reason| mismatch(reason, Some(&assigned_root), Some(&primary_root)))?;
    let primary_common_dir = required_git_common_dir(&primary_root, "registered primary checkout")
        .map_err(|reason| mismatch(reason, Some(&assigned_root), Some(&primary_root)))?;
    if assigned_common_dir != primary_common_dir {
        return Err(mismatch(
            format!(
                "assigned and registered primary checkouts have different Git common dirs ('{}' != '{}')",
                assigned_common_dir.display(),
                primary_common_dir.display()
            ),
            Some(&assigned_root),
            Some(&primary_root),
        ));
    }

    Ok(Some(DeclaredWorktreePair {
        requested_workspace_path: requested_workspace_path
            .unwrap_or_else(|| assigned_root.display().to_string()),
        requested_repo_root,
        assigned_root,
        primary_root,
    }))
}

fn declared_value_text(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn exact_canonical_dir(path: &Path, label: &str) -> Result<PathBuf, String> {
    if !path.is_dir() {
        return Err(format!("{label} '{}' is not a directory", path.display()));
    }
    path.canonicalize()
        .map_err(|error| format!("cannot canonicalize {label} '{}': {error}", path.display()))
}

fn required_git_top_level(path: &Path, label: &str) -> Result<PathBuf, String> {
    git_top_level(path)
        .map_err(|error| format!("cannot resolve Git top level for {label}: {error}"))?
        .ok_or_else(|| format!("{label} '{}' is not a Git checkout", path.display()))
}

fn required_git_common_dir(path: &Path, label: &str) -> Result<PathBuf, String> {
    git_common_dir(path)
        .map_err(|error| format!("cannot resolve Git common dir for {label}: {error}"))?
        .ok_or_else(|| {
            format!(
                "cannot resolve Git common dir for {label} '{}'",
                path.display()
            )
        })
}

#[allow(clippy::too_many_arguments)]
fn worktree_mismatch_error(
    input: &Value,
    task_ctx: Option<&Value>,
    run_id: &str,
    provider: &str,
    requested_workspace_path: Option<&str>,
    requested_repo_root: Option<&str>,
    assigned_root: Option<&Path>,
    primary_root: Option<&Path>,
    reason: String,
) -> DispatchError {
    let diagnostic = json!({
        "code": "worktree_mismatch",
        "reason": reason,
        "task_id": task_id(input, task_ctx),
        "run_id": run_id,
        "provider": provider,
        "requested_workspace_path": requested_workspace_path,
        "requested_repo_root": requested_repo_root,
        "resolved_assigned_root": assigned_root,
        "registered_primary_root": primary_root,
        "automatic_reconciliation": false,
    });
    DispatchError::WorktreeIntegrity {
        code: "worktree_mismatch",
        diagnostic: diagnostic.to_string(),
    }
}

/// Durable, content-bearing evidence written before a dirty integrity failure
/// can reach worktree cleanup.
#[derive(Debug, Clone, Serialize)]
struct WorktreeRecoveryArtifact {
    root: PathBuf,
    tracked_patch: PathBuf,
    untracked_payload: PathBuf,
    manifest: PathBuf,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeRecoveryManifest<'a> {
    schema_version: u8,
    task_id: &'a str,
    run_id: &'a str,
    recorded_head: &'a str,
    recorded_branch: &'a Option<String>,
    tracked_patch: &'static str,
    untracked_payload: &'static str,
    untracked_files: Vec<&'a String>,
}

/// Pre-spawn boundary guard for a linked-worktree provider invocation.
///
/// The registered primary checkout comes from the runtime's tool context; the
/// assigned checkout comes from the rendered `workspace_path`. The guard is
/// enabled only for a validated declared worktree pair. A direct invocation
/// may bypass the guard only when its cwd and registered root are the same
/// checkout; distinct roots without the pair fail before spawn.
pub(crate) struct WorktreeBoundaryGuard {
    task_id: String,
    run_id: String,
    provider: String,
    requested_workspace_path: String,
    requested_repo_root: Option<String>,
    assigned_root: PathBuf,
    primary_root: PathBuf,
    assigned_before: GitWorktreeFingerprint,
    primary_before: GitWorktreeFingerprint,
    rebase_recovery: Option<RebaseRecoveryCheckpoint>,
}

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
            primary_before: git_fingerprint(&primary_root)?,
            assigned_root,
            primary_root,
            rebase_recovery: None,
        }))
    }

    /// Compare both monitored checkouts after the provider reaches any
    /// terminal outcome. A primary delta is benign in exactly two shapes: a
    /// proven same-branch fast-forward, or a stationary HEAD whose only
    /// movement is Orbit record-store dirt. Both require that the delta left
    /// every path this run touched alone: linked worktrees keep their own
    /// HEAD, and the shipment rebase checkpoint owns reconciliation with a new
    /// base. Primary rewrites, primary branch switches, primary source edits,
    /// primary dirt overlapping the run, and unapproved history changes in the
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

        if primary_dirt_only_delta_is_benign(
            &self.primary_before,
            &primary_after,
            &primary_dirt_paths,
            &conflicting_paths,
        ) {
            tracing::info!(
                target: "orbit.engine.cli_runner",
                task_id = %self.task_id,
                run_id = %self.run_id,
                primary_head = %primary_after.head,
                ignored_primary_paths = ?primary_dirt_paths,
                "accepted concurrent primary record-store dirt disjoint from the run; primary HEAD and branch never moved"
            );
            return Ok(());
        }

        if primary_fast_forward_is_benign(
            &self.primary_root,
            &self.primary_before,
            &primary_after,
            &conflicting_paths,
        )? {
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

    fn preserve_dirty_assigned_worktree(
        &self,
        assigned_after: &GitWorktreeFingerprint,
    ) -> Result<Option<WorktreeRecoveryArtifact>, DispatchError> {
        // ADR-0299: content-bearing evidence must outlive forced removal of
        // the linked checkout, so it lives under the shared Git common dir.
        if assigned_after.dirty_paths.is_empty() {
            return Ok(None);
        }
        if self.run_id.is_empty()
            || !self
                .run_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(DispatchError::CliInvocationPermanent(format!(
                "cannot preserve dirty worktree for unsafe run id '{}'",
                self.run_id
            )));
        }

        let common_dir = git_common_dir(&self.assigned_root)?.ok_or_else(|| {
            DispatchError::CliInvocationPermanent(format!(
                "cannot preserve dirty worktree '{}': Git common dir is unavailable",
                self.assigned_root.display()
            ))
        })?;
        let recovery_parent = common_dir.join("orbit").join("worktree-recovery");
        let recovery_root = recovery_parent.join(&self.run_id);
        let artifact = WorktreeRecoveryArtifact {
            tracked_patch: recovery_root.join("tracked.patch"),
            untracked_payload: recovery_root.join("untracked"),
            manifest: recovery_root.join("manifest.json"),
            root: recovery_root.clone(),
        };
        if recovery_root.is_dir() {
            return Ok(Some(artifact));
        }

        fs::create_dir_all(&recovery_parent).map_err(|error| {
            recovery_io_error("create recovery parent", &recovery_parent, error)
        })?;
        let pending =
            recovery_parent.join(format!(".{}.{}.pending", self.run_id, std::process::id()));
        fs::create_dir(&pending)
            .map_err(|error| recovery_io_error("create pending recovery", &pending, error))?;
        let pending_payload = pending.join("untracked");
        fs::create_dir(&pending_payload).map_err(|error| {
            recovery_io_error("create untracked recovery payload", &pending_payload, error)
        })?;

        let patch = git_stdout_bytes(
            &self.assigned_root,
            &[
                "diff",
                "--binary",
                "--full-index",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "HEAD",
                "--",
            ],
        )?;
        let patch_path = pending.join("tracked.patch");
        fs::write(&patch_path, patch)
            .map_err(|error| recovery_io_error("write tracked patch", &patch_path, error))?;

        for relative in assigned_after.untracked_content.keys() {
            let relative_path = safe_relative_path(relative)?;
            let source = self.assigned_root.join(&relative_path);
            let destination = pending_payload.join(&relative_path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    recovery_io_error("create untracked payload directory", parent, error)
                })?;
            }
            fs::copy(&source, &destination).map_err(|error| {
                recovery_io_error("copy untracked recovery payload", &destination, error)
            })?;
        }

        let manifest = WorktreeRecoveryManifest {
            schema_version: 1,
            task_id: &self.task_id,
            run_id: &self.run_id,
            recorded_head: &assigned_after.head,
            recorded_branch: &assigned_after.branch,
            tracked_patch: "tracked.patch",
            untracked_payload: "untracked/",
            untracked_files: assigned_after.untracked_content.keys().collect(),
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| {
            DispatchError::CliInvocationPermanent(format!(
                "serialize worktree recovery manifest for run '{}': {error}",
                self.run_id
            ))
        })?;
        let manifest_path = pending.join("manifest.json");
        fs::write(&manifest_path, manifest_bytes).map_err(|error| {
            recovery_io_error("write worktree recovery manifest", &manifest_path, error)
        })?;
        fs::rename(&pending, &recovery_root).map_err(|error| {
            recovery_io_error("publish worktree recovery", &recovery_root, error)
        })?;
        Ok(Some(artifact))
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
            "assigned_before": self.assigned_before,
            "assigned_after": assigned_after,
            "primary_before": self.primary_before,
            "primary_after": primary_after,
            "recovery": recovery,
            "automatic_reconciliation": false,
        });
        DispatchError::WorktreeIntegrity {
            code,
            diagnostic: diagnostic.to_string(),
        }
    }
}

/// Orbit's own record store inside a checkout. Tasks, frictions, auto-tasks,
/// and routines under this prefix are rewritten continuously by the
/// engine that drives the pipeline and by out-of-run curation passes; they are
/// never part of a run's code candidate, so the primary's copy moving under a
/// stationary HEAD carries no data-loss signal.
const ORBIT_RECORD_STORE_PREFIX: &str = ".orbit/";

/// Accept a primary checkout that never moved but merely gained or lost record
/// store dirt away from the run.
///
/// `primary_fast_forward_is_benign` covers the case where the primary branch
/// advanced; it rejects `before.head == after.head` on its first clause, which
/// left a stationary primary with *any* unrelated dirt delta reported as
/// `primary_checkout_drift` (F2026-07-166: an out-of-run curation pass
/// rewriting tracked Orbit records killed a complete, validated implementation).
///
/// Unlike a fast-forward, a stationary HEAD offers no positive proof that Git
/// itself produced the delta, so acceptance is deliberately narrower than
/// "disjoint from the run" alone (ADR-0293): every mutated path must live in
/// the record store. A provider that escapes its worktree to edit source in
/// the primary — the ORB-10134 data-loss hazard — keeps failing closed even
/// when the file it touched is one this run never looked at. The delta must
/// also be fully explained by dirt-path movement; an otherwise unattributable
/// fingerprint change is not something this branch understands.
fn primary_dirt_only_delta_is_benign(
    before: &GitWorktreeFingerprint,
    after: &GitWorktreeFingerprint,
    primary_dirt_paths: &[String],
    conflicting_paths: &[String],
) -> bool {
    before.head == after.head
        && before.branch == after.branch
        && !primary_dirt_paths.is_empty()
        && conflicting_paths.is_empty()
        && primary_dirt_paths
            .iter()
            .all(|path| path.starts_with(ORBIT_RECORD_STORE_PREFIX))
}

fn primary_fast_forward_is_benign(
    root: &Path,
    before: &GitWorktreeFingerprint,
    after: &GitWorktreeFingerprint,
    conflicting_paths: &[String],
) -> Result<bool, DispatchError> {
    // Linked-worktree shipment owns clean base fast-forwards; the provider
    // boundary still rejects primary rewrites and any primary dirt that lands
    // on a path this run touched. Primary dirt disjoint from the run — a
    // concurrent Orbit process dropping an unrelated file, for instance — is
    // not interference and must not convert a benign base advance into
    // primary_checkout_drift (F2026-07-139).
    if before.head == after.head || before.branch != after.branch || !conflicting_paths.is_empty() {
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

fn declared_workspace_path(input: &Value, task_ctx: Option<&Value>) -> Option<String> {
    input
        .get("workspace_path")
        .and_then(Value::as_str)
        .or_else(|| {
            task_ctx
                .and_then(|task| task.get("workspace_path"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
}

fn task_id(input: &Value, task_ctx: Option<&Value>) -> String {
    input
        .get("task_id")
        .and_then(Value::as_str)
        .or_else(|| {
            task_ctx
                .and_then(|task| task.get("id"))
                .and_then(Value::as_str)
        })
        .unwrap_or("unknown")
        .to_string()
}

fn safe_relative_path(path: &str) -> Result<PathBuf, DispatchError> {
    let candidate = Path::new(path);
    if candidate.as_os_str().is_empty()
        || candidate
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(DispatchError::CliInvocationPermanent(format!(
            "cannot preserve unsafe untracked path '{path}'"
        )));
    }
    Ok(candidate.to_path_buf())
}

fn recovery_io_error(action: &str, path: &Path, error: std::io::Error) -> DispatchError {
    DispatchError::CliInvocationPermanent(format!("{action} at '{}': {error}", path.display()))
}

fn git_top_level(path: &Path) -> Result<Option<PathBuf>, DispatchError> {
    let output = git_output_raw(path, &["rev-parse", "--show-toplevel"])?;
    if !output.status.success() {
        return Ok(None);
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if root.is_empty() {
        return Ok(None);
    }
    Ok(Some(canonicalize_dir(Path::new(&root))))
}

fn git_common_dir(path: &Path) -> Result<Option<PathBuf>, DispatchError> {
    let output = git_output_raw(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    if !output.status.success() {
        return Ok(None);
    }
    let common = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if common.is_empty() {
        return Ok(None);
    }
    Ok(Some(canonicalize_dir(Path::new(&common))))
}
