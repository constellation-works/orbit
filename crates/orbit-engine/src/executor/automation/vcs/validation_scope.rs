//! Which commands required validation runs on a candidate [ORB-16784].
//!
//! A conflict or recovery repair that changed only prose need not rerun the
//! whole required list. When `workflow.unlocked_path_validation_commands` is
//! set, [`validation_scope`] compares the candidate's tree with the last tree
//! this run passed the full list on, as the host recorded it
//! ([`RuntimeHost::record_validated_tree`]). A difference confined to paths
//! matching `workflow.unlocked_context_paths` runs only the configured
//! commands; any other difference runs the full list. The decision is the
//! tree diff alone, never what an agent reports it changed: a rebase onto a
//! base that brought a code change differs from the validated tree in that
//! code, so it is a new combination and is validated in full.
//!
//! Every outcome is recorded in the step's output and in each command log as
//! `validation_scope`, naming the scope, why it was chosen, and the trees it
//! compared, so review and landing can audit it.

use std::path::Path;

use orbit_common::fs::path_glob::RelativePathGlob;
use serde_json::{Value, json};

use crate::context::{RuntimeHost, ValidatedTree};

use super::git::git_output_raw;

/// The most changed paths a scope record lists; the count is always exact.
const MAX_RECORDED_PATHS: usize = 50;

/// The commands this validation runs and why.
pub(super) struct ValidationScope {
    /// The commands to run: the full list, or the unlocked-path commands.
    pub(super) commands: Vec<String>,
    /// Whether `commands` is the full list, which a pass then records as
    /// the run's validated tree.
    pub(super) full: bool,
    /// The audit record: `kind`, `reason` and the trees compared.
    pub(super) record: Value,
}

/// Decide the scope for `tree`, the candidate's tree, given `full`, the full
/// command list (deferred-sandbox replays included). A host error reading
/// the record, or a tree diff Git cannot produce, falls back to the full
/// list: narrowing is an optimization and never a reason to fail.
pub(super) fn validation_scope<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    run_id: &str,
    task_ids: &[String],
    full: Vec<String>,
    tree: &str,
) -> ValidationScope {
    let full_scope = |reason: &str, extra: Value| {
        let mut record = json!({ "kind": "full", "reason": reason, "tested_tree": tree });
        if let (Some(record), Some(extra)) = (record.as_object_mut(), extra.as_object()) {
            record.extend(extra.clone());
        }
        ValidationScope {
            commands: full.clone(),
            full: true,
            record,
        }
    };
    let unlocked = host.unlocked_path_validation();
    if unlocked.commands.is_empty() {
        return full_scope("unlocked_path_validation_unset", json!({}));
    }
    // A claimed leaf's evidence belongs to its owner, which judges it.
    if host.worker_invocation().is_some() {
        return full_scope("claimed_leaf", json!({}));
    }
    let validated = match host.validated_tree(run_id) {
        Ok(Some(validated)) => validated,
        Ok(None) => return full_scope("no_validated_tree", json!({})),
        Err(error) => {
            return full_scope(
                "validated_tree_unreadable",
                json!({ "detail": error.to_string() }),
            );
        }
    };
    let compared = json!({
        "validated_tree": validated.tree,
        "validated_head": validated.head,
    });
    if !describes(&validated, run_id, task_ids, workspace_path, &full) {
        // The required list, the delivered tasks or the checkout changed
        // since: that validation is not this one's.
        return full_scope("validated_tree_out_of_scope", compared);
    }
    let changed = match changed_paths(workspace_path, &validated.tree, tree) {
        Ok(changed) => changed,
        Err(detail) => {
            let mut extra = compared;
            extra["detail"] = json!(detail);
            return full_scope("tree_diff_unavailable", extra);
        }
    };
    let globs = match unlocked
        .paths
        .iter()
        .map(|pattern| RelativePathGlob::new(pattern))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(globs) => globs,
        Err(error) => {
            let mut extra = compared;
            extra["detail"] = json!(error.to_string());
            return full_scope("unlocked_paths_invalid", extra);
        }
    };
    let locked = changed
        .iter()
        .filter(|path| !globs.iter().any(|glob| glob.matches(path)))
        .collect::<Vec<_>>();
    let mut extra = compared;
    extra["changed_path_count"] = json!(changed.len());
    extra["changed_paths"] = json!(changed.iter().take(MAX_RECORDED_PATHS).collect::<Vec<_>>());
    extra["unlocked_paths"] = json!(unlocked.paths);
    if !locked.is_empty() {
        extra["locked_path_count"] = json!(locked.len());
        extra["locked_paths"] = json!(
            locked
                .into_iter()
                .take(MAX_RECORDED_PATHS)
                .collect::<Vec<_>>()
        );
        return full_scope("changed_locked_paths", extra);
    }
    let mut record = json!({
        "kind": "narrowed",
        "reason": "unlocked_paths_only",
        "tested_tree": tree,
        "required_commands": full,
    });
    if let (Some(record), Some(extra)) = (record.as_object_mut(), extra.as_object()) {
        record.extend(extra.clone());
    }
    ValidationScope {
        commands: unlocked.commands,
        full: false,
        record,
    }
}

/// Whether `validated` is this run's validation of these tasks, in this
/// checkout, under this full command list.
fn describes(
    validated: &ValidatedTree,
    run_id: &str,
    task_ids: &[String],
    workspace_path: &Path,
    full: &[String],
) -> bool {
    validated.run_id == run_id
        && validated.task_ids == task_ids
        && Path::new(&validated.workspace_path) == workspace_path
        && validated.commands == full
}

/// Every path whose content differs between the two trees, renames as a
/// deletion and an addition, so both sides are judged.
fn changed_paths(workspace_path: &Path, from: &str, to: &str) -> Result<Vec<String>, String> {
    let output = git_output_raw(
        workspace_path,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            "--end-of-options",
            from,
            to,
        ],
    )
    .map_err(|error| error.to_string())?;
    Ok(output
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect())
}
