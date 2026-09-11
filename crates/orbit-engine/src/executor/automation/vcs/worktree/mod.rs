mod cleanup;
mod dependency_delivery;
mod gc;
mod merge;
mod setup;

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::executor::automation::input::{input_string_field, required_input_string};

use super::git::git_output;

/// Whether `path` is one of `repo_root`'s registered worktrees (main or
/// linked), comparing both the literal path Git recorded and its canonical
/// form. Any other checkout that happens to live at `path` is not ours to
/// clean or remove.
pub(super) fn is_registered_worktree(repo_root: &Path, path: &Path) -> Result<bool, OrbitError> {
    let list = git_output(repo_root, &["worktree", "list", "--porcelain"])?;
    let expected_literal = path.to_string_lossy();
    let expected_canonical = std::fs::canonicalize(path).ok();
    Ok(list
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|registered| {
            if registered == expected_literal {
                return true;
            }
            match (&expected_canonical, std::fs::canonicalize(registered).ok()) {
                (Some(expected), Some(registered)) => *expected == registered,
                _ => false,
            }
        }))
}

pub use gc::{WorktreeGcOptions, WorktreeGcResult, collect_worktrees};
pub(in crate::executor::automation) use merge::merge_batch_worktree_into_base;
pub(in crate::executor::automation) use setup::setup_worktree;

const SHARED_WORKTREE_NAME_PREFIX: &str = "parallel-batch";
const DEFAULT_BRANCH_PREFIX: &str = "orbit";

/// What a run's worktree is: the tasks it serves, the branch prefix that
/// names it, and the run token that makes its directory unique.
///
/// `setup_worktree` creates a worktree from this identity; the garbage
/// collector re-derives the same identity from the stored run record to
/// recognise the directory on disk. The two sites used to spell the rule out
/// independently and silently drifted — gc probed a singular `task_id` while
/// `task_pr_pipeline` emits a `task_ids` array — so gc matched no real
/// directory, classified every worktree `skipped:unrecognized`, and reclaimed
/// nothing (ORB-10427). This type is the single derivation; add new inputs
/// here rather than at either call site.
pub(in crate::executor::automation) struct WorktreeIdentity {
    /// Every task the worktree serves, in input order. Non-empty.
    pub(in crate::executor::automation) task_ids: Vec<String>,
    /// The branch (and directory) prefix, `orbit` unless overridden.
    pub(in crate::executor::automation) branch_prefix: String,
    /// The token that names the directory alongside the prefix.
    pub(in crate::executor::automation) run_id: String,
}

impl WorktreeIdentity {
    /// Derive the identity from a `setup_worktree`-shaped input.
    ///
    /// `engine_run_id` is the id the engine knows the run by, consulted when
    /// the input itself carries no `run_id`. `setup_worktree` passes `None`:
    /// it only ever sees its own input, and falls back to a task-derived
    /// token. gc passes the run record's id, because a stored `initial_input`
    /// does not carry the `run_id` the engine injected at dispatch.
    ///
    /// Fails when the input names no task at all — such a run never went
    /// through `setup_worktree`.
    pub(in crate::executor::automation) fn from_input(
        input: &Value,
        engine_run_id: Option<&str>,
    ) -> Result<Self, OrbitError> {
        let task_ids = task_ids_from_input(input)?;
        // The stored epic_pipeline input is the one worktree-owning shape
        // whose task and naming fields are supplied to the nested worktree
        // step rather than persisted on the run itself.
        let epic_run_id =
            input_string_field(input, "epic_task_id").map(|task_id| format!("epic-{task_id}"));
        let run_id = input_string_field(input, "run_id")
            .or(epic_run_id)
            .or_else(|| {
                engine_run_id
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| fallback_run_id_for_tasks(&task_ids));
        let branch_prefix = input_string_field(input, "branch_prefix")
            .or_else(|| input_string_field(input, "epic_task_id").map(|_| "epic".to_string()))
            .unwrap_or_else(|| DEFAULT_BRANCH_PREFIX.to_string());
        Ok(Self {
            task_ids,
            branch_prefix,
            run_id,
        })
    }

    /// The worktree directory this identity resolves to.
    pub(in crate::executor::automation) fn path(
        &self,
        repo_root: &Path,
    ) -> Result<PathBuf, OrbitError> {
        resolve_worktree_path_from_prefix(repo_root, &self.branch_prefix, &self.run_id)
    }

    /// The directory setup would have chosen had no `run_id` reached it, when
    /// that differs from [`Self::path`].
    ///
    /// gc consults this as a second candidate so a worktree created under the
    /// task-derived fallback token is still recognised: setup prefers
    /// `input.run_id` and falls back to `task-<id>` / `bundle-<hash>`, and gc
    /// cannot tell after the fact which branch setup took.
    pub(in crate::executor::automation) fn fallback_path(
        &self,
        repo_root: &Path,
    ) -> Result<Option<PathBuf>, OrbitError> {
        let fallback = fallback_run_id_for_tasks(&self.task_ids);
        if fallback == self.run_id {
            return Ok(None);
        }
        resolve_worktree_path_from_prefix(repo_root, &self.branch_prefix, &fallback).map(Some)
    }
}

fn task_ids_from_input(input: &Value) -> Result<Vec<String>, OrbitError> {
    if let Some(items) = input.get("task_ids").and_then(Value::as_array) {
        let task_ids = items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| {
                        OrbitError::InvalidInput(
                            "setup_worktree input.task_ids entries must be non-empty strings"
                                .to_string(),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !task_ids.is_empty() {
            return Ok(task_ids);
        }
    }

    if let Some(epic_task_id) = input_string_field(input, "epic_task_id") {
        return Ok(vec![epic_task_id]);
    }

    Ok(vec![required_input_string(input, "task_id")?.to_string()])
}

fn fallback_run_id_for_tasks(task_ids: &[String]) -> String {
    if task_ids.len() == 1 {
        return format!("task-{}", task_ids[0]);
    }

    let mut sorted_ids = task_ids.to_vec();
    sorted_ids.sort();
    let digest = Sha256::digest(sorted_ids.join(","));
    format!("bundle-{}", &format!("{digest:x}")[..8])
}

/// Extract the `run_id` from an activity input value, returning a trimmed
/// non-empty string. Used by activities that need to resolve the shared
/// worktree for a run.
fn require_run_id<'a>(input: &'a Value, activity: &str) -> Result<&'a str, OrbitError> {
    input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OrbitError::InvalidInput(format!("{activity} requires input.run_id")))
}

pub(in crate::executor::automation) fn sanitize_worktree_token(
    value: &str,
) -> Result<String, OrbitError> {
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
            "run_id '{value}' sanitizes to an empty string"
        )));
    }
    Ok(trimmed)
}

pub fn resolve_worktree_path_from_prefix(
    repo_root: &Path,
    prefix: &str,
    run_id: &str,
) -> Result<PathBuf, OrbitError> {
    let sanitized = sanitize_worktree_token(run_id)?;
    let dir_name = format!("{prefix}-{sanitized}");
    match worktree_root() {
        Some(root) => Ok(root.join(repo_name(repo_root)?).join(dir_name)),
        None => Ok(repo_root
            .join(".orbit")
            .join("state")
            .join("worktrees")
            .join(dir_name)),
    }
}

pub fn resolve_shared_worktree_path(repo_root: &Path, run_id: &str) -> Result<PathBuf, OrbitError> {
    let dir_name = shared_worktree_dir_name(run_id)?;
    match worktree_root() {
        Some(root) => Ok(root.join(repo_name(repo_root)?).join(dir_name)),
        None => Ok(repo_root
            .join(".orbit")
            .join("state")
            .join("worktrees")
            .join(dir_name)),
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

#[cfg(test)]
mod tests;
