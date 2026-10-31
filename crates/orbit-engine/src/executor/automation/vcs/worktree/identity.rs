use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::identity::lower_hex;
use orbit_types::workflow::DEFAULT_BRANCH_PREFIX;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::executor::automation::input::{input_string_field, required_input_string};

use super::paths::{
    reject_unsafe_branch_prefix, resolve_worktree_path_from_prefix, sanitize_worktree_component,
};

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
    /// Branch namespace, `orbit` unless overridden. Slashes stay in the Git
    /// ref (`team/x/<task>-<hash>`). The directory name is the sanitized
    /// prefix, so the checkout is one child of the worktree root.
    pub(in crate::executor::automation) branch_prefix: String,
    /// The token that names the directory alongside the prefix.
    pub(in crate::executor::automation) run_id: String,
}

/// Every top-level input key worktree identity derivation
/// (`WorktreeIdentity::from_input`) reads, so a run inventory for worktree GC
/// can hydrate only these (`JobRunQuery::input_keys`). Keep it in step with
/// that derivation.
pub const WORKTREE_IDENTITY_INPUT_KEYS: &[&str] = &[
    "task_ids",
    "task_id",
    "epic_task_id",
    "run_id",
    "branch_prefix",
];

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
    /// through `setup_worktree` — or when `branch_prefix` contains `..`,
    /// starts with `-`, or sanitizes to an empty directory name.
    pub(in crate::executor::automation) fn from_input(
        input: &Value,
        engine_run_id: Option<&str>,
    ) -> Result<Self, OrbitError> {
        let task_ids = task_ids_from_input(input)?;
        // Historical decoding only [ORB-12491]: `epic_pipeline` is retired and
        // nothing emits this shape any more, but its stored inputs named the
        // worktree through `epic_task_id` rather than persisting `run_id` on
        // the run itself. GC re-derives identities from stored run records, so
        // dropping this would strand every worktree an epic run left behind.
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
        // Reject before any Git call or mkdir. A slash stays in the branch
        // namespace; path derivation sanitizes it to one directory component.
        reject_unsafe_branch_prefix(&branch_prefix)?;
        sanitize_worktree_component("branch_prefix", &branch_prefix)?;
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
    ) -> Result<std::path::PathBuf, OrbitError> {
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
    ) -> Result<Option<std::path::PathBuf>, OrbitError> {
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

    // Historical decoding only, alongside the `epic-` run token above: a
    // retired `epic_pipeline` run record names its task this way and GC must
    // still recognize the worktree it left behind [ORB-12491].
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
    format!("bundle-{}", &lower_hex(digest)[..8])
}

/// Extract the `run_id` from an activity input value, returning a trimmed
/// non-empty string. Used by activities that need to resolve the shared
/// worktree for a run.
pub(super) fn require_run_id<'a>(input: &'a Value, activity: &str) -> Result<&'a str, OrbitError> {
    input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OrbitError::InvalidInput(format!("{activity} requires input.run_id")))
}
