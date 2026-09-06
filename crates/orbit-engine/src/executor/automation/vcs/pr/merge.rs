use std::path::Path;

use orbit_common::OrbitError;
use orbit_store::pr_scoreboard;
use orbit_types::task::{ExternalRef, Task, TaskStatus};
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::super::super::input::{
    canonicalize_existing_dir, input_string_field, required_job_run_id,
};
use super::super::freshness::ensure_branch_fresh_against_base;
use super::super::git::{base_sync_mode_from_input, git_command_success, git_output};
use super::super::operations;
use super::attribution::ship_done_attribution;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MergeStrategy {
    Squash,
    Rebase,
    Merge,
}

/// Repository capabilities needed to request a permitted PR merge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct MergeCapabilities {
    pub(super) strategy: MergeStrategy,
    pub(super) auto_merge_allowed: bool,
}

impl MergeStrategy {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Squash => "squash",
            Self::Rebase => "rebase",
            Self::Merge => "merge",
        }
    }
}

pub(super) fn resolve_merge_strategy<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &str,
    pr_number: &str,
) -> Result<MergeStrategy, OrbitError> {
    Ok(resolve_merge_capabilities(host, workspace_path, pr_number)?.strategy)
}

pub(super) fn resolve_merge_capabilities<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &str,
    pr_number: &str,
) -> Result<MergeCapabilities, OrbitError> {
    let response = host.run_private_vcs_operation(
        operations::PR_MERGE_CAPABILITIES,
        json!({
            "pr": pr_number,
            "workspace_path": workspace_path,
        }),
    )?;
    let repository = response.get("repository").ok_or_else(|| {
        OrbitError::Execution(
            "merge strategy resolution: private VCS response omitted repository capabilities"
                .to_string(),
        )
    })?;
    let capability = |key: &str| {
        repository.get(key).and_then(Value::as_bool).ok_or_else(|| {
            OrbitError::Execution(format!(
                "merge strategy resolution: repository capabilities omitted boolean {key}"
            ))
        })
    };
    let squash = capability("allow_squash_merge")?;
    let rebase = capability("allow_rebase_merge")?;
    let merge = capability("allow_merge_commit")?;
    let linear = capability("requires_linear_history")?;
    let auto_merge_allowed = capability("allow_auto_merge")?;

    if squash {
        return Ok(MergeCapabilities {
            strategy: MergeStrategy::Squash,
            auto_merge_allowed,
        });
    }
    if rebase {
        return Ok(MergeCapabilities {
            strategy: MergeStrategy::Rebase,
            auto_merge_allowed,
        });
    }
    if merge && !linear {
        return Ok(MergeCapabilities {
            strategy: MergeStrategy::Merge,
            auto_merge_allowed,
        });
    }

    let repository_name = repository
        .get("name_with_owner")
        .and_then(Value::as_str)
        .unwrap_or("repository");
    let base_branch = repository
        .get("base_branch")
        .and_then(Value::as_str)
        .unwrap_or("target branch");
    Err(OrbitError::Execution(format!(
        "no permitted merge method for pull request #{pr_number} in {repository_name}: \
         squash={squash}, rebase={rebase}, merge_commit={merge}, \
         {base_branch}.requires_linear_history={linear}; repository settings were not changed \
         and no administrative bypass was attempted"
    )))
}

pub(in crate::executor::automation) fn git_merge<H: RuntimeHost + Sync + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let batch_id = required_job_run_id(input, "git_merge")?;
    if host
        .list_tasks_filtered(None, None, None, Some(batch_id), None, None)?
        .is_empty()
    {
        return Ok(json!({}));
    }

    let strategy = input
        .get("strategy")
        .and_then(Value::as_str)
        .unwrap_or("fast_forward");
    match strategy {
        "fast_forward" => super::super::worktree::merge_batch_worktree_into_base(host, input),
        "pr_merge" => merge_batch_pr(host, input),
        other => Err(OrbitError::InvalidInput(format!(
            "git_merge: unknown strategy '{other}'; expected fast_forward or pr_merge"
        ))),
    }
}

pub(super) fn merge_batch_pr<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let batch_id = required_job_run_id(input, "merge_batch_pr")?;

    let batch_tasks = host.list_tasks_filtered(None, None, None, Some(batch_id), None, None)?;
    if batch_tasks.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "merge_batch_pr: no tasks found for job_run_id '{batch_id}'"
        )));
    }

    // Find the GitHub PR external ref from the first task that has one.
    let pr_number = batch_tasks
        .iter()
        .find_map(Task::github_pr_number)
        .ok_or_else(|| {
            OrbitError::InvalidInput(
                "merge_batch_pr: no task in batch has a github-pr external ref".to_string(),
            )
        })?
        .to_string();

    let workspace_path = resolve_batch_workspace_path(host, input, batch_id)?;

    // Get the current branch from the workspace
    let head = git_output(&workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let head = head.trim().to_string();
    let base = input_string_field(input, "base").unwrap_or_else(|| "main".to_string());
    let base_sync_mode = base_sync_mode_from_input(input)?;

    // Check that ALL tasks have APPROVED pr_status
    for task in &batch_tasks {
        let pr_status_raw = task.pr_status.as_deref().unwrap_or("none");
        let review_decision = super::super::super::review::normalize_review_decision(pr_status_raw);
        if review_decision != "APPROVED" {
            return Err(OrbitError::Execution(format!(
                "task '{}' is not approved (pr_status={pr_status_raw})",
                task.id
            )));
        }
    }

    // Check that ALL tasks are in Review or Done status
    for task in &batch_tasks {
        if !matches!(task.status, TaskStatus::Review | TaskStatus::Done) {
            return Err(OrbitError::Execution(format!(
                "task '{}' must be in Review or Done before merge_batch_pr; current status is {}",
                task.id, task.status
            )));
        }
    }

    ensure_branch_fresh_against_base(&workspace_path, &head, &base, base_sync_mode)?;

    let merge_strategy =
        resolve_merge_strategy(host, &workspace_path.to_string_lossy(), &pr_number)?;
    host.run_private_vcs_operation(
        operations::PR_MERGE,
        json!({
            "pr": pr_number,
            "strategy": merge_strategy.as_str(),
            "workspace_path": workspace_path,
        }),
    )?;

    // Best-effort remote branch cleanup.  Some repos have GitHub's
    // "Automatically delete head branches" enabled, so the remote ref may
    // already be gone — ignore errors.
    let _ = git_command_success(&workspace_path, &["push", "origin", "--delete", &head]);

    let batch_requires_revision = batch_tasks
        .iter()
        .map(|task| task_required_revision(host, task))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .any(|requires_revision| requires_revision);
    let batch_author = batch_tasks.iter().find_map(ship_done_attribution);

    // Preserve ship attribution per task across the Review -> Done transition.
    // See `merge_batch_pr_preserves_task_attribution_per_task`: the source of
    // truth is task.implemented_by -> task.created_by -> system fallback.
    for task in &batch_tasks {
        host.apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                status: if task.status == TaskStatus::Review {
                    Some(TaskStatus::Done)
                } else {
                    None
                },
                external_refs: vec![ExternalRef::github_pr(pr_number.clone())?],
                model: ship_done_attribution(task),
                ..TaskAutomationUpdate::default()
            },
        )?;
    }

    if host.scoring_enabled()
        && let Some(model) = batch_author
    {
        let _ = if batch_requires_revision {
            pr_scoreboard::record_pr_count_with_revision(host.scoreboard_dir(), &model)
        } else {
            pr_scoreboard::record_pr_count_without_revision(host.scoreboard_dir(), &model)
        };
    }

    Ok(json!({
        "merged": true,
        "strategy": merge_strategy.as_str(),
    }))
}

fn resolve_batch_workspace_path<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    batch_id: &str,
) -> Result<std::path::PathBuf, OrbitError> {
    match input_string_field(input, "workspace_path") {
        Some(path) => canonicalize_existing_dir(&path, "workspace_path"),
        None => {
            let repo_root = host.repo_root()?;
            super::super::worktree::resolve_shared_worktree_path(Path::new(&repo_root), batch_id)
        }
    }
}

fn task_required_revision<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &Task,
) -> Result<bool, OrbitError> {
    let history = host.get_task_history(&task.id)?;
    Ok(history.iter().any(|entry| {
        entry.event == "status_changed"
            && entry.from_status == Some(TaskStatus::Review)
            && matches!(
                entry.to_status,
                Some(TaskStatus::Backlog | TaskStatus::InProgress | TaskStatus::Rejected)
            )
    }))
}
