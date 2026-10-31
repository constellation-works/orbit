use std::path::{Component, Path, PathBuf};

use orbit_common::OrbitError;

const SHARED_WORKTREE_NAME_PREFIX: &str = "parallel-batch";

pub(in crate::executor::automation) fn sanitize_worktree_token(
    value: &str,
) -> Result<String, OrbitError> {
    sanitize_worktree_component("run_id", value)
}

/// One worktree directory component. `/`, `\`, and every other character
/// outside `[A-Za-z0-9._-]` become `-`. Leading and trailing `-` and `.`
/// are stripped so the result cannot be `.`, `..`, or a flag-like name.
pub(super) fn sanitize_worktree_component(label: &str, value: &str) -> Result<String, OrbitError> {
    let sanitized: String = value
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = sanitized
        .trim_matches(|c: char| c == '-' || c == '.')
        .to_string();
    if trimmed.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "{label} '{value}' sanitizes to an empty string"
        )));
    }
    Ok(trimmed)
}

/// `..` would leave the worktree root if the prefix were joined raw, and a
/// leading `-` would be another argv word to `git worktree add -b`. A slash
/// is a real branch namespace (`team/x/<task>`); the directory form is
/// sanitized separately.
pub(super) fn reject_unsafe_branch_prefix(prefix: &str) -> Result<(), OrbitError> {
    if prefix.contains("..") {
        return Err(OrbitError::InvalidInput(format!(
            "branch_prefix '{prefix}' must not contain '..'"
        )));
    }
    if prefix.starts_with('-') {
        return Err(OrbitError::InvalidInput(format!(
            "branch_prefix '{prefix}' must not start with '-'"
        )));
    }
    Ok(())
}

pub fn resolve_worktree_path_from_prefix(
    repo_root: &Path,
    prefix: &str,
    run_id: &str,
) -> Result<PathBuf, OrbitError> {
    let prefix = sanitize_worktree_component("branch_prefix", prefix)?;
    let run_id = sanitize_worktree_component("run_id", run_id)?;
    worktree_directory(repo_root, &format!("{prefix}-{run_id}"))
}

pub fn resolve_shared_worktree_path(repo_root: &Path, run_id: &str) -> Result<PathBuf, OrbitError> {
    worktree_directory(repo_root, &shared_worktree_dir_name(run_id)?)
}

/// Where a short-lived detached checkout (a base rerun, host evidence) goes:
/// the managed worktree root of the primary checkout that owns `repo`'s Git
/// common directory, never the common directory itself. The Linux Git
/// protection scan refuses any symlink under the common directory, and a
/// checkout of a repository with symlinked files is not Git metadata.
pub(crate) fn scratch_checkout_path(repo: &Path, dir_name: &str) -> Result<PathBuf, OrbitError> {
    let common = orbit_common::fs::git::git_common_dir(repo)?;
    let primary = common
        .parent()
        .filter(|_| common.file_name().is_some_and(|name| name == ".git"))
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "Git common directory '{}' is not a primary checkout's `.git`",
                common.display()
            ))
        })?;
    worktree_directory(primary, dir_name)
}

/// Join `dir_name` as one child of the worktree root. A name that is not a
/// single normal component, or a join that does not stay under that root,
/// is rejected.
fn worktree_directory(repo_root: &Path, dir_name: &str) -> Result<PathBuf, OrbitError> {
    let parent = match worktree_root() {
        Some(root) => root.join(repo_name(repo_root)?),
        None => repo_root.join(".orbit").join("state").join("worktrees"),
    };
    if !is_single_worktree_component(dir_name) {
        return Err(OrbitError::InvalidInput(format!(
            "worktree directory '{dir_name}' must be a single component under '{}'",
            parent.display()
        )));
    }
    let path = parent.join(dir_name);
    if path.parent() != Some(parent.as_path()) {
        return Err(OrbitError::InvalidInput(format!(
            "worktree path '{}' escapes '{}'",
            path.display(),
            parent.display()
        )));
    }
    Ok(path)
}

fn is_single_worktree_component(dir_name: &str) -> bool {
    let mut components = Path::new(dir_name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(name)), None) => {
            let name = name.to_string_lossy();
            !name.is_empty() && !name.starts_with('-') && !name.starts_with('.')
        }
        _ => false,
    }
}

fn worktree_root() -> Option<PathBuf> {
    std::env::var("ORBIT_WORKTREE_ROOT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn repo_name(repo_root: &Path) -> Result<&str, OrbitError> {
    repo_root
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "cannot derive repository name from '{}'",
                repo_root.display()
            ))
        })
}

fn shared_worktree_dir_name(run_id: &str) -> Result<String, OrbitError> {
    Ok(format!(
        "{SHARED_WORKTREE_NAME_PREFIX}-{}",
        sanitize_worktree_token(run_id)?
    ))
}
