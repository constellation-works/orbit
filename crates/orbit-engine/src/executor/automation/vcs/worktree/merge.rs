use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::context::RuntimeHost;
use crate::executor::automation::input::{canonicalize_existing_dir, input_string_field};

use super::super::git::{
    BaseSyncMode, GitTimeoutBudget, GitTimeoutBudgetGuard, base_sync_mode_from_input,
    git_command_success, git_failure_error, git_output, git_output_raw, git_run, git_success,
    git_timeout_error, resolve_worktree_start_point,
};
use super::super::handoff::rebase_in_progress;
use super::resolve_shared_worktree_path;

const DEFAULT_BASE: &str = "main";
const MAX_REBASE_RETRY_ATTEMPTS: usize = 2;

pub(in crate::executor::automation) fn merge_batch_worktree_into_base<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let _timeout_budget = GitTimeoutBudgetGuard::install(GitTimeoutBudget::from_input(input)?);
    let run_id = super::require_run_id(input, "merge_batch_worktree_into_base")?;
    let repo_root_str = host.repo_root()?;
    let repo_root = canonicalize_existing_dir(&repo_root_str, "repo_root")?;
    let workspace_path = match input_string_field(input, "workspace_path") {
        Some(path) => canonicalize_existing_dir(&path, "workspace_path")?,
        None => resolve_shared_worktree_path(&repo_root, run_id)?,
    };

    ensure_clean_checkout(&workspace_path, "shared batch worktree")?;

    let workspace_branch = git_output(&workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let workspace_branch = workspace_branch.trim().to_string();
    if workspace_branch == "HEAD" {
        return Err(OrbitError::Execution(
            "merge_batch_worktree_into_base: shared worktree is in detached HEAD state".to_string(),
        ));
    }

    let base = input_string_field(input, "base").unwrap_or_else(|| DEFAULT_BASE.to_string());
    let base_sync_mode = base_sync_mode_from_input(input)?;
    let base_checkout = checkout_holding_branch(&repo_root, &base)?.unwrap_or(repo_root.clone());
    ensure_clean_checkout(&base_checkout, "base branch checkout")?;
    merge_with_rebase_retry(
        &|before, after| {
            host.record_direct_landing_intent(
                &orbit_types::workflow::automation::DirectLandingRequest {
                    run_id: run_id.into(),
                    branch: base.clone(),
                    before_commit: before,
                    after_commit: after,
                },
            )
        },
        &repo_root,
        &base_checkout,
        &workspace_path,
        &base,
        &workspace_branch,
        base_sync_mode,
    )?;

    Ok(json!({
        "base": base,
        "workspace_path": workspace_path.to_string_lossy().to_string(),
        "workspace_branch": workspace_branch,
    }))
}

fn checkout_base_branch(
    repo_root: &Path,
    base: &str,
    start_point: &str,
    base_sync_mode: BaseSyncMode,
) -> Result<(), OrbitError> {
    if git_command_success(
        repo_root,
        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
    )? {
        git_success(repo_root, &["checkout", base])?;
        if base_sync_mode == BaseSyncMode::Remote {
            fast_forward_local_base_to_remote(repo_root, base, start_point)?;
        }
        return Ok(());
    }

    git_success(repo_root, &["checkout", "-B", base, start_point])?;
    Ok(())
}

fn merge_with_rebase_retry(
    record_intent: &dyn Fn(String, String) -> Result<(), OrbitError>,
    repo_root: &Path,
    base_checkout: &Path,
    workspace_path: &Path,
    base: &str,
    workspace_branch: &str,
    base_sync_mode: BaseSyncMode,
) -> Result<(), OrbitError> {
    for attempt in 0..=MAX_REBASE_RETRY_ATTEMPTS {
        let start_point = resolve_worktree_start_point(repo_root, base, base_sync_mode)?;
        if base_checkout == repo_root {
            checkout_base_branch(repo_root, base, &start_point, base_sync_mode)?;
        } else {
            let checked_out = git_output(base_checkout, &["rev-parse", "--abbrev-ref", "HEAD"])?;
            if checked_out.trim() != base {
                return Err(OrbitError::Execution(format!(
                    "linked base worktree '{}' moved from branch '{base}' to '{}'",
                    base_checkout.display(),
                    checked_out.trim()
                )));
            }
            if base_sync_mode == BaseSyncMode::Remote {
                fast_forward_local_base_to_remote(base_checkout, base, &start_point)?;
            }
        }
        let before = git_output(base_checkout, &["rev-parse", "HEAD"])?;
        let after = git_output(workspace_path, &["rev-parse", "HEAD"])?;
        if before.trim() != after.trim()
            && git_command_success(
                repo_root,
                &["merge-base", "--is-ancestor", before.trim(), after.trim()],
            )?
        {
            record_intent(before.trim().into(), after.trim().into())?;
        }
        if git_command_success(base_checkout, &["merge", "--ff-only", workspace_branch])? {
            return Ok(());
        }
        if attempt == MAX_REBASE_RETRY_ATTEMPTS {
            return Err(OrbitError::Execution(format!(
                "merge_batch_worktree_into_base: failed to fast-forward merge '{workspace_branch}' into '{base}' after {} rebase retry attempts",
                MAX_REBASE_RETRY_ATTEMPTS
            )));
        }

        let updated_base = resolve_worktree_start_point(repo_root, base, base_sync_mode)?;
        let rebase_already = rebase_in_progress(workspace_path)?;
        let rebase_outcome = git_run(workspace_path, &["rebase", &updated_base])?;
        if rebase_outcome.timed_out || !rebase_outcome.success {
            if !rebase_already {
                let _ = git_success(workspace_path, &["rebase", "--abort"]);
            }
            if rebase_outcome.timed_out {
                return Err(OrbitError::Execution(format!(
                    "{}; merge_batch_worktree_into_base: Git rebase timed out. {} This is timeout recovery, not conflict or failure-handoff recovery.",
                    git_timeout_error(
                        workspace_path,
                        &["rebase", &updated_base],
                        rebase_outcome.timeout_ms,
                        &rebase_outcome.stderr,
                    ),
                    if rebase_already {
                        "Pre-existing rebase state was left intact."
                    } else {
                        "The rebase started by this attempt was aborted."
                    }
                )));
            }
            if rebase_already {
                return Err(OrbitError::Execution(format!(
                    "merge_batch_worktree_into_base: a pre-existing rebase is in progress; not aborting. {}",
                    git_failure_error(
                        workspace_path,
                        &["rebase", &updated_base],
                        &rebase_outcome.stderr,
                    )
                )));
            }
            return Err(git_failure_error(
                workspace_path,
                &["rebase", &updated_base],
                &rebase_outcome.stderr,
            ));
        }
        ensure_clean_checkout(workspace_path, "shared batch worktree")?;
    }

    Ok(())
}

/// Locate the linked worktree that already has `base` checked out. Git refuses
/// to check the same branch out in the primary checkout, so stacked local
/// pipelines must merge directly in the owning worktree (the epic worktree in
/// particular).
pub(super) fn checkout_holding_branch(
    repo_root: &Path,
    base: &str,
) -> Result<Option<PathBuf>, OrbitError> {
    let listing = git_output(repo_root, &["worktree", "list", "--porcelain"])?;
    let expected_branch = format!("refs/heads/{base}");
    let mut current_path = None;
    for line in listing.lines().chain(std::iter::once("")) {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(PathBuf::from(path));
        } else if let Some(branch) = line.strip_prefix("branch ")
            && branch == expected_branch
        {
            return Ok(current_path);
        } else if line.is_empty() {
            current_path = None;
        }
    }
    Ok(None)
}

fn fast_forward_local_base_to_remote(
    repo_root: &Path,
    base: &str,
    remote_base: &str,
) -> Result<(), OrbitError> {
    let divergence = git_output(
        repo_root,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{remote_base}...{base}"),
        ],
    )?;
    let mut parts = divergence.split_whitespace();
    let remote_only = parse_divergence_count(parts.next(), "remote-only", remote_base, base)?;
    let local_only = parse_divergence_count(parts.next(), "local-only", remote_base, base)?;
    if parts.next().is_some() {
        return Err(OrbitError::Execution(format!(
            "unexpected git divergence output while comparing '{base}' to '{remote_base}': {divergence}"
        )));
    }

    if local_only > 0 {
        return Err(OrbitError::Execution(format!(
            "local base branch '{base}' has {local_only} commit(s) not present in '{remote_base}'; reconcile it or run with base_sync=local"
        )));
    }

    if remote_only > 0 {
        git_success(repo_root, &["merge", "--ff-only", remote_base])?;
    }

    Ok(())
}

fn parse_divergence_count(
    value: Option<&str>,
    label: &str,
    left: &str,
    right: &str,
) -> Result<u64, OrbitError> {
    let raw = value.ok_or_else(|| {
        OrbitError::Execution(format!(
            "missing {label} divergence count while comparing '{left}' to '{right}'"
        ))
    })?;
    raw.parse::<u64>().map_err(|error| {
        OrbitError::Execution(format!(
            "invalid {label} divergence count '{raw}' while comparing '{left}' to '{right}': {error}"
        ))
    })
}

/// Uses [`git_output_raw`] rather than [`git_output`]: the latter trims the
/// whole output, which would misalign the index/worktree columns of a
/// single-line result by one byte (see `git_output`'s doc comment).
pub(super) fn ensure_clean_checkout(path: &Path, label: &str) -> Result<(), OrbitError> {
    let status = git_output_raw(path, &["status", "--porcelain"])?;
    if status.trim().is_empty() {
        return Ok(());
    }

    let has_unmerged = status.lines().any(|line| {
        let bytes = line.as_bytes();
        if bytes.len() < 2 {
            return false;
        }
        let x = bytes[0] as char;
        let y = bytes[1] as char;
        x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D')
    });
    if has_unmerged {
        return Err(OrbitError::Execution(format!(
            "{label} '{}' has unresolved merge conflicts",
            path.display()
        )));
    }

    Err(OrbitError::Execution(format!(
        "{label} '{}' must be clean before merge_batch_worktree_into_base",
        path.display()
    )))
}
