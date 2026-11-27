use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::context::RuntimeHost;
use crate::executor::automation::input::{canonicalize_existing_dir, input_string_field};

use super::super::git::{
    BaseSyncMode, GitTimeoutBudget, GitTimeoutBudgetGuard, base_sync_mode_from_input,
    git_command_success, git_failure_error, git_output, git_output_bytes, git_output_paths,
    git_output_raw, git_run, git_success, git_timeout_error, normalize_base_branch,
    resolve_worktree_start_point, timeout_recovery_error,
};
use super::super::handoff::rebase_in_progress;
use super::resolve_shared_worktree_path;

const DEFAULT_BASE: &str = "main";
const MAX_REBASE_RETRY_ATTEMPTS: usize = 2;

/// Base tip before a successful fast-forward, and the commit the base is at
/// afterwards. They match when Git moved nothing, including an ancestor
/// candidate. `after` reflects any rebase retry.
struct FastForwardLanding {
    before: String,
    after: String,
}

/// The base branch a batch worktree fast-forwards, the checkout holding it,
/// and the worktree branch that lands.
struct FastForwardTarget<'a> {
    repo_root: &'a Path,
    base_checkout: &'a Path,
    workspace_path: &'a Path,
    base: &'a str,
    workspace_branch: &'a str,
    base_sync_mode: BaseSyncMode,
}

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
        None => resolve_shared_worktree_path(&host.process_env(), &repo_root, run_id)?,
    };

    ensure_clean_checkout(
        &host.process_env(),
        &workspace_path,
        "shared batch worktree",
    )?;

    let workspace_branch = git_output(
        &host.process_env(),
        &workspace_path,
        &["rev-parse", "--abbrev-ref", "HEAD"],
    )?;
    let workspace_branch = workspace_branch.trim().to_string();
    if workspace_branch == "HEAD" {
        return Err(OrbitError::Execution(
            "merge_batch_worktree_into_base: shared worktree is in detached HEAD state".to_string(),
        ));
    }

    // Same spelling rule as the worktree start point: `origin/main` names the
    // local `main` branch this step lands on, not a detached remote ref.
    let base = normalize_base_branch(
        &input_string_field(input, "base").unwrap_or_else(|| DEFAULT_BASE.to_string()),
    )?;
    let base_sync_mode = base_sync_mode_from_input(input)?;
    let base_checkout = checkout_holding_branch(&host.process_env(), &repo_root, &base)?
        .unwrap_or(repo_root.clone());
    ensure_base_checkout_ready(&host.process_env(), &base_checkout, None)?;
    let request = |before, after| orbit_types::workflow::automation::DirectLandingRequest {
        run_id: run_id.into(),
        branch: base.clone(),
        before_commit: before,
        after_commit: after,
        task_ids: vec![],
        handoff_id: None,
    };
    let landed = merge_with_rebase_retry(
        &host.process_env(),
        &|before, after| host.record_direct_landing_intent(&request(before, after)),
        FastForwardTarget {
            repo_root: &repo_root,
            base_checkout: &base_checkout,
            workspace_path: &workspace_path,
            base: &base,
            workspace_branch: &workspace_branch,
            base_sync_mode,
        },
    )?;
    // A moved branch is a fact even if recording its diff stats fails.
    let FastForwardLanding { before, after } = landed;
    if before != after
        && let Err(error) = host.record_direct_landing(&request(before.clone(), after.clone()))
    {
        tracing::warn!(%error, run_id, "recording the local landing's diff stats failed");
    }

    let mut output = json!({
        "merged": before != after,
        "already_contained": before == after,
        "completion_held": false,
        "base": base,
        "base_commit": after,
        "workspace_path": workspace_path.to_string_lossy().to_string(),
        "workspace_branch": workspace_branch,
    });
    if before != after {
        output["landed_commit"] = json!(after);
    }
    Ok(output)
}

fn checkout_base_branch(
    process_env: &orbit_common::Env,
    repo_root: &Path,
    base: &str,
    start_point: &str,
    base_sync_mode: BaseSyncMode,
) -> Result<(), OrbitError> {
    if git_command_success(
        process_env,
        repo_root,
        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
    )? {
        git_success(process_env, repo_root, &["checkout", base])?;
        if base_sync_mode == BaseSyncMode::Remote {
            fast_forward_local_base_to_remote(process_env, repo_root, base, start_point)?;
        }
        return Ok(());
    }

    git_success(
        process_env,
        repo_root,
        &["checkout", "-B", base, start_point],
    )?;
    Ok(())
}

fn merge_with_rebase_retry(
    process_env: &orbit_common::Env,
    record_intent: &dyn Fn(String, String) -> Result<(), OrbitError>,
    target: FastForwardTarget<'_>,
) -> Result<FastForwardLanding, OrbitError> {
    let FastForwardTarget {
        repo_root,
        base_checkout,
        workspace_path,
        base,
        workspace_branch,
        base_sync_mode,
    } = target;
    for attempt in 0..=MAX_REBASE_RETRY_ATTEMPTS {
        let start_point =
            resolve_worktree_start_point(process_env, repo_root, base, base_sync_mode)?;
        if base_checkout == repo_root {
            checkout_base_branch(process_env, repo_root, base, &start_point, base_sync_mode)?;
        } else {
            let checked_out = git_output(
                process_env,
                base_checkout,
                &["rev-parse", "--abbrev-ref", "HEAD"],
            )?;
            if checked_out.trim() != base {
                return Err(OrbitError::Execution(format!(
                    "linked base worktree '{}' moved from branch '{base}' to '{}'",
                    base_checkout.display(),
                    checked_out.trim()
                )));
            }
            if base_sync_mode == BaseSyncMode::Remote {
                fast_forward_local_base_to_remote(process_env, base_checkout, base, &start_point)?;
            }
        }
        let before = git_output(process_env, base_checkout, &["rev-parse", "HEAD"])?;
        let after = git_output(process_env, workspace_path, &["rev-parse", "HEAD"])?;
        if before.trim() != after.trim()
            && crate::executor::automation::vcs::git::is_ancestor(
                process_env,
                repo_root,
                before.trim(),
                after.trim(),
            )?
        {
            record_intent(before.trim().into(), after.trim().into())?;
        }
        // Recheck on every attempt, after base synchronization and intent
        // recording: operator files can appear while the candidate is built.
        ensure_base_checkout_ready(process_env, base_checkout, Some(after.trim()))?;
        if git_command_success(
            process_env,
            base_checkout,
            &["merge", "--ff-only", "--no-overwrite-ignore", after.trim()],
        )? {
            let actual = git_output(process_env, base_checkout, &["rev-parse", "HEAD"])?;
            let actual = actual.trim();
            if actual != before.trim() && actual != after.trim() {
                return Err(OrbitError::Execution(
                    "merge_batch_worktree_into_base: base moved to an unexpected commit during merge".to_string(),
                ));
            }
            return Ok(FastForwardLanding {
                before: before.trim().to_string(),
                after: actual.to_string(),
            });
        }
        if attempt == MAX_REBASE_RETRY_ATTEMPTS {
            return Err(OrbitError::Execution(format!(
                "merge_batch_worktree_into_base: failed to fast-forward merge '{workspace_branch}' into '{base}' after {} rebase retry attempts",
                MAX_REBASE_RETRY_ATTEMPTS
            )));
        }

        let updated_base =
            resolve_worktree_start_point(process_env, repo_root, base, base_sync_mode)?;
        let rebase_already = rebase_in_progress(process_env, workspace_path)?;
        let rebase_outcome = git_run(process_env, workspace_path, &["rebase", &updated_base])?;
        if rebase_outcome.timed_out || !rebase_outcome.success {
            if !rebase_already {
                let _ = git_success(process_env, workspace_path, &["rebase", "--abort"]);
            }
            if rebase_outcome.timed_out {
                return Err(timeout_recovery_error(
                    rebase_outcome.timeout_ms,
                    format!(
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
                    ),
                ));
            }
            if rebase_already {
                return Err(OrbitError::Execution(format!(
                    "merge_batch_worktree_into_base: a pre-existing rebase is in progress; not aborting. {}",
                    git_failure_error(
                        workspace_path,
                        &["rebase", &updated_base],
                        rebase_outcome.exit_code,
                        &rebase_outcome.stderr,
                    )
                )));
            }
            return Err(git_failure_error(
                workspace_path,
                &["rebase", &updated_base],
                rebase_outcome.exit_code,
                &rebase_outcome.stderr,
            ));
        }
        ensure_clean_checkout(process_env, workspace_path, "shared batch worktree")?;
    }

    Err(OrbitError::Execution(
        "merge_batch_worktree_into_base: fast-forward ended without a landed commit".to_string(),
    ))
}

/// Locate the linked worktree that already has `base` checked out. Git refuses
/// to check the same branch out in the primary checkout, so stacked local
/// pipelines must merge directly in the owning worktree.
pub(in crate::executor::automation::vcs) fn checkout_holding_branch(
    process_env: &orbit_common::Env,
    repo_root: &Path,
    base: &str,
) -> Result<Option<PathBuf>, OrbitError> {
    let listing = git_output_bytes(
        process_env,
        repo_root,
        &["worktree", "list", "--porcelain", "-z"],
    )?;
    let expected_branch = format!("refs/heads/{base}");
    let mut current_path = None;
    for record in listing.split(|byte| *byte == 0) {
        if let Some(path) = record.strip_prefix(b"worktree ") {
            current_path = Some(path);
        } else if let Some(branch) = record.strip_prefix(b"branch ")
            && branch == expected_branch.as_bytes()
        {
            return current_path.map(worktree_path_from_bytes).transpose();
        } else if record.is_empty() {
            current_path = None;
        }
    }
    Ok(None)
}

#[cfg(unix)]
fn worktree_path_from_bytes(path: &[u8]) -> Result<PathBuf, OrbitError> {
    use std::os::unix::ffi::OsStringExt;
    Ok(PathBuf::from(std::ffi::OsString::from_vec(path.to_vec())))
}

#[cfg(not(unix))]
fn worktree_path_from_bytes(path: &[u8]) -> Result<PathBuf, OrbitError> {
    let path = std::str::from_utf8(path).map_err(|error| {
        OrbitError::Execution(format!("invalid worktree path from Git: {error}"))
    })?;
    Ok(PathBuf::from(path))
}

fn fast_forward_local_base_to_remote(
    process_env: &orbit_common::Env,
    repo_root: &Path,
    base: &str,
    remote_base: &str,
) -> Result<(), OrbitError> {
    let divergence = git_output(
        process_env,
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
        ensure_base_checkout_ready(process_env, repo_root, Some(remote_base))?;
        git_success(
            process_env,
            repo_root,
            &["merge", "--ff-only", "--no-overwrite-ignore", remote_base],
        )?;
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
pub(in crate::executor::automation::vcs) fn ensure_clean_checkout(
    process_env: &orbit_common::Env,
    path: &Path,
    label: &str,
) -> Result<(), OrbitError> {
    let status = git_output_raw(
        process_env,
        path,
        &["status", "--porcelain", "--untracked-files=all"],
    )?;
    ensure_clean_status(path, label, &status)
}

/// Setup has no candidate yet, so it checks tracked work only. Landing also
/// checks untracked paths against the incoming tree's changes, without
/// requiring the operator to remove unrelated notes from the base checkout.
pub(in crate::executor::automation::vcs) fn ensure_base_checkout_ready(
    process_env: &orbit_common::Env,
    path: &Path,
    incoming: Option<&str>,
) -> Result<(), OrbitError> {
    let status = git_output_raw(
        process_env,
        path,
        &["status", "--porcelain", "--untracked-files=no"],
    )?;
    ensure_clean_status(path, "base branch checkout", &status)?;
    let Some(incoming) = incoming else {
        return Ok(());
    };
    let changed = git_output_paths(
        process_env,
        path,
        &[
            "diff",
            "--no-renames",
            "--name-only",
            "-z",
            "HEAD",
            incoming,
            "--",
        ],
    )?;
    // Scan incoming paths and any non-directory parent obstructing them.
    // Avoid enumerating unrelated ignored files beneath parent directories.
    let mut selectors = std::collections::BTreeSet::new();
    for name in &changed {
        let incoming = Path::new(name);
        selectors.insert(format!(":(literal){name}"));
        for ancestor in incoming
            .ancestors()
            .skip(1)
            .take_while(|p| !p.as_os_str().is_empty())
        {
            if std::fs::symlink_metadata(path.join(ancestor)).is_ok_and(|m| m.file_type().is_dir())
            {
                continue;
            }
            selectors.insert(format!(":(literal){}", ancestor.to_string_lossy()));
        }
    }
    let mut untracked = Vec::new();
    for chunk in selectors.into_iter().collect::<Vec<_>>().chunks(100) {
        let mut args = vec!["ls-files", "--others", "-z", "--"];
        args.extend(chunk.iter().map(String::as_str));
        untracked.extend(git_output_paths(process_env, path, &args)?);
    }
    untracked.sort();
    untracked.dedup();
    let conflicts: Vec<_> = untracked
        .iter()
        .filter(|untracked| {
            let untracked = Path::new(untracked);
            changed.iter().any(|changed| {
                let changed = Path::new(changed);
                untracked.starts_with(changed) || changed.starts_with(untracked)
            })
        })
        // Debug quoting keeps embedded newlines and other unusual characters
        // from masquerading as additional entries in the diagnostic.
        .map(|path| format!("{path:?}"))
        .collect();
    if conflicts.is_empty() {
        return Ok(());
    }
    Err(OrbitError::Execution(format!(
        "base branch checkout '{}' has untracked paths conflicting with the incoming changes:\n{}\nMove or commit these conflicting paths, then retry shipping.",
        path.display(),
        conflicts.join("\n"),
    )))
}

fn ensure_clean_status(path: &Path, label: &str, status: &str) -> Result<(), OrbitError> {
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
            "{label} '{}' has unresolved merge conflicts:\n{status}\nResolve and commit the conflicts before shipping.",
            path.display(),
        )));
    }

    Err(OrbitError::Execution(format!(
        "{label} '{}' must be clean before merge_batch_worktree_into_base. Dirty paths (git status --short):\n{status}\nCommit or stash these changes, and remove unwanted untracked files, then retry shipping.",
        path.display(),
    )))
}
