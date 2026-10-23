use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::task::{GITHUB_PR_EXTERNAL_REF_SYSTEM, TaskStatus};
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::super::super::input::{
    canonicalize_existing_dir, input_string_field, required_job_run_id,
};
use super::super::freshness::ensure_branch_fresh_against_base;
use super::super::git::{base_sync_mode_from_input, git_command_success, git_output};
use super::super::operations;
use super::answers::MergeCapabilities;
use super::attribution::ship_done_attribution;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::executor::automation::vcs) enum MergeStrategy {
    Squash,
    Rebase,
    Merge,
}

/// The merge method this run may request, with the provider's own name for
/// the repository it applies to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::executor::automation::vcs) struct PermittedMerge {
    pub(in crate::executor::automation::vcs) strategy: MergeStrategy,
    pub(in crate::executor::automation::vcs) auto_merge_allowed: bool,
    /// `owner/name` as the provider reports it. The owner landing consumer
    /// compares this to the repository the handoff claims [ORB-12499].
    pub(in crate::executor::automation::vcs) repository: String,
}

impl MergeStrategy {
    pub(in crate::executor::automation::vcs) const fn as_str(self) -> &'static str {
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
    Ok(resolve_permitted_merge(host, workspace_path, pr_number)?.strategy)
}

pub(in crate::executor::automation::vcs) fn resolve_permitted_merge<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &str,
    pr_number: &str,
) -> Result<PermittedMerge, OrbitError> {
    let response = host.run_private_vcs_operation(
        operations::PR_MERGE_CAPABILITIES,
        json!({
            "pr": pr_number,
            "workspace_path": workspace_path,
        }),
    )?;
    let capabilities = MergeCapabilities::from_response(&response)?;
    let squash = capabilities.allow_squash_merge;
    let rebase = capabilities.allow_rebase_merge;
    let merge = capabilities.allow_merge_commit;
    let linear = capabilities.requires_linear_history;
    let permitted = |strategy| {
        let repository = capabilities.repository.clone().ok_or_else(|| {
            OrbitError::Execution(
                "merge strategy resolution: repository capabilities omitted name_with_owner"
                    .to_string(),
            )
        })?;
        Ok(PermittedMerge {
            strategy,
            auto_merge_allowed: capabilities.allow_auto_merge,
            repository,
        })
    };

    if squash {
        return permitted(MergeStrategy::Squash);
    }
    if rebase {
        return permitted(MergeStrategy::Rebase);
    }
    if merge && !linear {
        return permitted(MergeStrategy::Merge);
    }

    let repository_name = capabilities.repository.as_deref().unwrap_or("repository");
    let base_branch = capabilities
        .base_branch
        .as_deref()
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
    if host.list_run_tasks(batch_id)?.is_empty() {
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

    let batch_tasks = host.list_run_tasks(batch_id)?;
    if batch_tasks.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "merge_batch_pr: no tasks found for job_run_id '{batch_id}'"
        )));
    }

    // Find the GitHub PR external ref from the first task that has one. The
    // ref is re-recorded on every task below, link included when known.
    let pr_ref = batch_tasks
        .iter()
        .find_map(|task| {
            task.external_refs
                .iter()
                .find(|reference| reference.system == GITHUB_PR_EXTERNAL_REF_SYSTEM)
        })
        .cloned()
        .ok_or_else(|| {
            OrbitError::InvalidInput(
                "merge_batch_pr: no task in batch has a github-pr external ref".to_string(),
            )
        })?;
    let pr_number = pr_ref.id.clone();

    let workspace_path = resolve_batch_workspace_path(host, input, batch_id)?;

    // Get the current branch from the workspace
    let head = git_output(&workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let head = head.trim().to_string();
    // The full commit the checks below judge. The provider merges only this
    // head, so a push that lands before the merge request is refused.
    let reviewed_head_sha = git_output(&workspace_path, &["rev-parse", "HEAD"])?;
    let reviewed_head_sha = reviewed_head_sha.trim().to_string();
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
            "reviewed_head_sha": reviewed_head_sha,
            "workspace_path": workspace_path,
        }),
    )?;

    // Best-effort remote branch cleanup.  Some repos have GitHub's
    // "Automatically delete head branches" enabled, so the remote ref may
    // already be gone — ignore errors.
    let _ = git_command_success(&workspace_path, &["push", "origin", "--delete", &head]);

    // Preserve ship attribution per task across the Review -> Done transition.
    // The source of truth is task.implemented_by -> task.created_by -> system
    // fallback (see `ship_done_attribution`).
    // Unresolved post-deploy criteria survive candidate reuse by a fresh run.
    for task in &batch_tasks {
        let completes = task.status == TaskStatus::Review
            && host.outstanding_owed_criteria(&task.id, None)?.is_empty();
        host.apply_task_automation_update(
            &task.id,
            TaskAutomationUpdate {
                status: completes.then_some(TaskStatus::Done),
                external_refs: vec![pr_ref.clone()],
                model: ship_done_attribution(task),
                ..TaskAutomationUpdate::default()
            },
        )?;
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
