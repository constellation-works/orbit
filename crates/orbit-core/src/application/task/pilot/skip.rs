//! An operator's skip of the task pilot: the `no-pilot` tag [ORB-21238].
//!
//! Filed on a task, `no-pilot` keeps automatic preparation from assessing it.
//! Admission also respects it: a task carrying the tag bypasses a stale
//! assessment and ordinary findings its latest receipt recorded. A current
//! `host_operational` hold still applies until a trusted operator decision or
//! newer assessment resolves the owner-side condition. Unverified review
//! findings, readiness gaps, context locks, dependencies and delivery holds
//! still apply.
//!
//! Adding the tag to a task a pilot has assessed, or one a pilot is preparing,
//! bypasses that gate, so it is an operator decision: any other writer is
//! refused, never stripped. The operator's write records a `pilot_skipped`
//! history entry naming the actor and the requirements digest it dispatches
//! without assessment. Filing a task with the tag stays open to every caller.

use std::path::{Component, Path};

use orbit_common::OrbitError;
use orbit_common::fs::selector::{
    Selector, anchor_path, canonical_selector_in_workspace, exists_in_workspace,
};
use orbit_types::task::{
    NO_AUTO_APPROVE_TAG, ReadinessGap, ReadinessStage, Task, TaskComplexity, TaskHistoryEntry,
    readiness_gaps,
};
use serde_json::{Value, json};

use super::super::TaskUpdateParams;
use super::super::update::TaskFieldWriter;
use crate::OrbitRuntime;
use crate::application::task::TaskAutomationPort;

/// The tag an operator adds to dispatch a task without a new pilot.
const NO_PILOT_TAG: &str = "no-pilot";

/// History event an operator's pilot skip records.
const PILOT_SKIPPED_EVENT: &str = "pilot_skipped";

/// Withhold/exclusion reason recorded when a task skips the pilot under
/// `workflow.task_pilot.skip_when_context_filled`.
pub(crate) const CONTEXT_FILES_FILLED_SKIP_REASON: &str = "context_files_filled";

/// Tags that always require the task pilot even when `context_files` is filled
/// and `workflow.task_pilot.skip_when_context_filled` is `true`.
const CONTEXT_FILLED_SKIP_EXCLUDED_TAGS: [&str; 3] =
    ["design", "breaking-change", NO_AUTO_APPROVE_TAG];

/// Whether `tags` carry the operator's pilot skip.
pub(super) fn skips_pilot(tags: &[String]) -> bool {
    tags.iter().any(|tag| tag == NO_PILOT_TAG)
}

/// Whether a task's metadata qualifies for the default filled-context
/// task-pilot skip (`workflow.task_pilot.skip_when_context_filled`):
/// non-empty `context_files`, `low` or `medium` complexity, none of the
/// exclusion tags (`design`, `breaking-change`, `no-auto-approve`), and no
/// blocking proposed-stage readiness gap.
pub(crate) fn task_qualifies_for_context_filled_pilot_skip(
    tags: &[String],
    context_files: &[String],
    complexity: Option<TaskComplexity>,
) -> bool {
    !context_files.is_empty()
        && matches!(
            complexity,
            Some(TaskComplexity::Low | TaskComplexity::Medium)
        )
        && !tags
            .iter()
            .any(|tag| CONTEXT_FILLED_SKIP_EXCLUDED_TAGS.contains(&tag.as_str()))
        && readiness_gaps(ReadinessStage::Proposed, tags, context_files, complexity)
            .iter()
            .all(|gap| !ReadinessGap::is_blocking(gap))
}

/// Whether every selector in `selectors` parses as a filesystem-anchored
/// selector (`file:`, `dir:`, or `symbol:`) and resolves to an existing target
/// of matching file/directory kind.
///
/// Checked against the Git tree at `revision` (or the workspace's local base
/// ref when `revision` is `None`) when the workspace is a Git repository,
/// falling back to the workspace filesystem only when it is not. Any parse
/// failure, traversal, symlink, missing target, or kind mismatch returns
/// `false` so the task pilot runs.
pub(crate) fn all_context_selectors_resolve(
    runtime: &OrbitRuntime,
    selectors: &[String],
    revision: Option<&str>,
) -> bool {
    if selectors.is_empty() {
        return false;
    }
    let mut parsed = Vec::with_capacity(selectors.len());
    for raw in selectors {
        let trimmed = raw.trim_start();
        let Ok(selector) = trimmed.parse::<Selector>() else {
            return false;
        };
        match selector {
            Selector::Dir { path } => parsed.push((path, true)),
            Selector::File { path } | Selector::Symbol { path, .. } => {
                parsed.push((path, false));
            }
            Selector::Module { .. } | Selector::Command { .. } => return false,
        }
    }

    let repo_root = &runtime.paths().repo_root;
    if let Ok(repo) = git2::Repository::open(repo_root) {
        let Some(tree) = resolve_git_tree(&repo, runtime.workspace_base_branch(), revision) else {
            return false;
        };
        return parsed
            .iter()
            .all(|(path, is_dir)| git_tree_selector_resolves(&tree, path, *is_dir));
    }

    let Ok(canonical_workspace) = repo_root.canonicalize() else {
        return false;
    };
    selectors.iter().all(|raw| {
        let trimmed = raw.trim_start();
        let Ok(canonical) = canonical_selector_in_workspace(trimmed, &canonical_workspace) else {
            return false;
        };
        if !exists_in_workspace(&canonical, &canonical_workspace) {
            return false;
        }
        let Ok(anchor) = anchor_path(&canonical) else {
            return false;
        };
        let resolved = canonical_workspace.join(anchor);
        if canonical.starts_with("dir:") {
            resolved.is_dir()
        } else {
            resolved.is_file()
        }
    })
}

fn resolve_git_tree<'r>(
    repo: &'r git2::Repository,
    base_branch: &str,
    revision: Option<&str>,
) -> Option<git2::Tree<'r>> {
    if let Some(revision) = revision {
        return repo
            .revparse_single(revision)
            .and_then(|object| object.peel_to_tree())
            .ok();
    }
    let local = repo
        .find_reference(&format!("refs/heads/{base_branch}"))
        .and_then(|reference| reference.peel_to_commit())
        .ok();
    let origin = repo
        .find_reference(&format!("refs/remotes/origin/{base_branch}"))
        .and_then(|reference| reference.peel_to_commit())
        .ok();
    let commit = match (local, origin) {
        (Some(local), Some(origin)) => {
            if local.id() == origin.id()
                || repo
                    .graph_descendant_of(local.id(), origin.id())
                    .unwrap_or(false)
            {
                local
            } else {
                origin
            }
        }
        (Some(local), None) => local,
        (None, Some(origin)) => origin,
        (None, None) => repo.head().and_then(|head| head.peel_to_commit()).ok()?,
    };
    commit.tree().ok()
}

fn git_tree_selector_resolves(tree: &git2::Tree<'_>, path: &str, is_dir: bool) -> bool {
    if path == "." {
        return is_dir;
    }
    if path.is_empty() || path.starts_with('/') {
        return false;
    }
    let candidate = Path::new(path);
    if candidate
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return false;
    }
    let Ok(entry) = tree.get_path(candidate) else {
        return false;
    };
    if is_dir {
        entry.kind() == Some(git2::ObjectType::Tree)
    } else {
        entry.kind() == Some(git2::ObjectType::Blob)
            && matches!(entry.filemode(), 0o100644 | 0o100755)
    }
}

/// Whether the default filled-context skip
/// (`workflow.task_pilot.skip_when_context_filled`) skips piloting `task` at
/// `revision`: config is enabled, the task has never been piloted, its
/// metadata qualifies, and all `context_files` selectors resolve.
pub(crate) fn context_filled_pilot_skip(
    runtime: &OrbitRuntime,
    task: &Task,
    revision: Option<&str>,
) -> Result<bool, OrbitError> {
    if !runtime.task_pilot_skip_when_context_filled()
        || !task_qualifies_for_context_filled_pilot_skip(
            &task.tags,
            &task.context_files,
            task.complexity,
        )
    {
        return Ok(false);
    }
    if runtime.task_pilot_assessed(&task.id)? {
        return Ok(false);
    }
    Ok(all_context_selectors_resolve(
        runtime,
        &task.context_files,
        revision,
    ))
}

/// The remedy text naming both ways past a pilot hold on `task`: a new
/// assessment, or the operator's skip.
pub(super) fn pilot_hold_remedies(task: &Task) -> String {
    let mut tags = task
        .tags
        .iter()
        .filter(|tag| tag.as_str() != NO_PILOT_TAG)
        .cloned()
        .collect::<Vec<_>>();
    tags.push(NO_PILOT_TAG.to_string());
    format!(
        "Run `orbit task repilot {id}` on the owner, or have an operator skip the pilot by \
         adding the `no-pilot` tag (`orbit task update {id} --tag {tags}`; `--tag` replaces \
         the list).",
        id = task.id,
        tags = tags.join(","),
    )
}

/// Whether `params` adds `no-pilot` to `task`.
fn adds_no_pilot(task: &Task, tags: Option<&[String]>) -> bool {
    tags.is_some_and(skips_pilot) && !skips_pilot(&task.tags)
}

/// What an operator's skip bypasses: the latest receipt and the requirements
/// it sealed, and the live runs preparing the task.
struct PilotSkipTarget {
    receipt: Option<String>,
    assessed_requirements: Value,
    pending_runs: Vec<String>,
}

impl OrbitRuntime {
    /// The pilot state `no-pilot` would bypass on `task`, or `None` when no
    /// pilot has assessed or is preparing it.
    fn pilot_skip_target(&self, task: &Task) -> Result<Option<PilotSkipTarget>, OrbitError> {
        let comments = self.get_task_pilot_comments(&task.id)?;
        let latest = super::pilot_receipts(&comments).next();
        let receipt = latest.map(|(_, receipt)| receipt.operation_id.to_string());
        // An undecodable receipt still counts as an assessment; it only has
        // no sealed digest to report.
        let assessed_requirements = latest
            .and_then(|(_, receipt)| serde_json::from_str::<Value>(receipt.body).ok())
            .and_then(|audit| audit.get("assessed_requirements").cloned())
            .unwrap_or(Value::Null);
        let pending_runs = self
            .pilot_preparation_holders()?
            .remove(&task.id)
            .map(|runs| runs.into_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        Ok(
            (receipt.is_some() || !pending_runs.is_empty()).then_some(PilotSkipTarget {
                receipt,
                assessed_requirements,
                pending_runs,
            }),
        )
    }

    /// Refuse a writer other than an operator that adds `no-pilot` (in the
    /// normalized `tags` it would store) to a task a pilot has assessed or is
    /// preparing.
    pub(in crate::application::task) fn refuse_unauthorized_pilot_skip(
        &self,
        task: &Task,
        tags: &[String],
        writer: TaskFieldWriter,
    ) -> Result<(), OrbitError> {
        if writer == TaskFieldWriter::Operator
            || !adds_no_pilot(task, Some(tags))
            || self.pilot_skip_target(task)?.is_none()
        {
            return Ok(());
        }
        Err(OrbitError::CapabilityDenied(format!(
            "only an operator can add `{NO_PILOT_TAG}` to task {}: a pilot has assessed or is \
             preparing it, and the tag dispatches it without a new assessment. Ask an operator \
             to skip the pilot, or request `orbit task repilot {}`",
            task.id, task.id
        )))
    }

    /// The `pilot_skipped` history entry for an operator's write that adds
    /// `no-pilot` to an assessed or preparing task: the actor, and the
    /// requirements digest this write leaves, which dispatch proceeds on
    /// without assessment.
    pub(in crate::application::task) fn pilot_skip_history(
        &self,
        task: &Task,
        params: &TaskUpdateParams,
        actor: &str,
    ) -> Result<Option<TaskHistoryEntry>, OrbitError> {
        if !adds_no_pilot(task, params.tags.as_deref()) {
            return Ok(None);
        }
        let Some(target) = self.pilot_skip_target(task)? else {
            return Ok(None);
        };
        let mut written = task.clone();
        if let Some(description) = &params.description {
            written.description.clone_from(description);
        }
        if let Some(criteria) = &params.acceptance_criteria {
            written.acceptance_criteria.clone_from(criteria);
        }
        let note = json!({
            "tag": NO_PILOT_TAG,
            "requirements_digest": super::freshness::pilot_requirements_material(&written),
            "pilot_operation_id": target.receipt,
            "assessed_requirements": target.assessed_requirements,
            "pending_pilot_runs": target.pending_runs,
        });
        Ok(Some(TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: actor.to_string(),
            event: PILOT_SKIPPED_EVENT.to_string(),
            note: Some(note.to_string()),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: None,
            surface: None,
            caller: None,
        }))
    }
}
