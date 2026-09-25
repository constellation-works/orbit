use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::fingerprint::git_output_raw;
use super::{DeclaredWorktreePair, DispatchError, canonicalize_dir};

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

pub(super) fn exact_canonical_dir(path: &Path, label: &str) -> Result<PathBuf, String> {
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
pub(super) fn worktree_mismatch_error(
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

pub(super) fn declared_workspace_path(input: &Value, task_ctx: Option<&Value>) -> Option<String> {
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

pub(super) fn task_id(input: &Value, task_ctx: Option<&Value>) -> String {
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

pub(super) fn git_top_level(path: &Path) -> Result<Option<PathBuf>, DispatchError> {
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

pub(super) fn git_common_dir(path: &Path) -> Result<Option<PathBuf>, DispatchError> {
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
