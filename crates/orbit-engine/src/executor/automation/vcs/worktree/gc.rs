use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::{JobRun, JobRunState};
use serde::Serialize;
use serde_json::Value;

use crate::context::RuntimeHost;
use crate::executor::automation::vcs::git::git_command;

use super::super::git::git_success;
use super::cleanup::remove_worktree;
use super::{WorktreeIdentity, is_registered_worktree, resolve_shared_worktree_path};

/// Task statuses that settle the work as done — the only statuses that
/// license discarding a run's worktree and branch. Every other status
/// (including `blocked` and `review`) retains it, and an unresolvable or
/// missing task id retains it as well: the collector fails closed.
fn task_status_permits_deletion(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Rejected | TaskStatus::Archived | TaskStatus::Done
    )
}

#[derive(Debug, Clone, Default)]
pub struct WorktreeGcOptions {
    pub delete: bool,
    pub run_id: Option<String>,
    pub older_than: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeGcReport {
    pub path: PathBuf,
    pub run_id: Option<String>,
    pub run_state: Option<JobRunState>,
    pub task_id: Option<String>,
    pub task_status: Option<TaskStatus>,
    pub pr_status: Option<String>,
    pub action: String,
    pub bytes_reclaimed: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeGcResult {
    pub dry_run: bool,
    pub bytes_reclaimed: u64,
    pub reports: Vec<WorktreeGcReport>,
}

pub fn collect_worktrees<H: RuntimeHost + ?Sized>(
    repo_root: &Path,
    runs: &[JobRun],
    task_host: &H,
    options: &WorktreeGcOptions,
) -> Result<WorktreeGcResult, OrbitError> {
    let mut known_paths = BTreeMap::<PathBuf, Vec<&JobRun>>::new();
    for run in runs {
        for path in expected_paths(repo_root, run)? {
            known_paths.entry(path).or_default().push(run);
        }
    }

    let mut reports = Vec::new();
    for (path, matching_runs) in &known_paths {
        let selected_runs = matching_runs
            .iter()
            .copied()
            .filter(|run| {
                options
                    .run_id
                    .as_deref()
                    .is_none_or(|wanted| wanted == run.run_id)
            })
            .collect::<Vec<_>>();
        if selected_runs.is_empty() {
            continue;
        }
        if !path.exists() {
            continue;
        }
        if matching_runs.len() > 1 {
            reports.extend(selected_runs.into_iter().map(|run| WorktreeGcReport {
                path: path.clone(),
                run_id: Some(run.run_id.clone()),
                run_state: Some(run.state),
                task_id: None,
                task_status: None,
                pr_status: None,
                action: "skipped:ambiguous_run_path".to_string(),
                bytes_reclaimed: 0,
            }));
            continue;
        }
        reports.push(classify_known(
            repo_root,
            path,
            selected_runs[0],
            task_host,
            options,
        )?);
    }

    if options.run_id.is_none() {
        let known: BTreeSet<_> = known_paths.keys().cloned().collect();
        for entry in on_disk_worktrees(repo_root)? {
            if !known.contains(&entry) {
                reports.push(WorktreeGcReport {
                    path: entry,
                    run_id: None,
                    run_state: None,
                    task_id: None,
                    task_status: None,
                    pr_status: None,
                    action: "skipped:unrecognized".to_string(),
                    bytes_reclaimed: 0,
                });
            }
        }
    }

    // This repairs already-stale Git administration entries. It is safe in
    // dry-run mode because it never removes a worktree directory or branch.
    git(repo_root, &["worktree", "prune"])?;

    reports.sort_by(|left, right| left.path.cmp(&right.path));
    let bytes_reclaimed = reports.iter().map(|report| report.bytes_reclaimed).sum();
    Ok(WorktreeGcResult {
        dry_run: !options.delete,
        bytes_reclaimed,
        reports,
    })
}

fn classify_known<H: RuntimeHost + ?Sized>(
    repo_root: &Path,
    path: &Path,
    run: &JobRun,
    task_host: &H,
    options: &WorktreeGcOptions,
) -> Result<WorktreeGcReport, OrbitError> {
    let task_ids = attributed_task_ids(run);
    let resolved = task_ids
        .iter()
        .map(|task_id| (task_id.clone(), task_host.get_task(task_id).ok()))
        .collect::<Vec<(String, Option<Task>)>>();
    let first_task = resolved.first().and_then(|(_, task)| task.as_ref());

    let mut report = WorktreeGcReport {
        path: path.to_path_buf(),
        run_id: Some(run.run_id.clone()),
        run_state: Some(run.state),
        // A bundle worktree serves several tasks; name all of them until a
        // single one is identified as the reason it is retained.
        task_id: (!task_ids.is_empty()).then(|| task_ids.join(",")),
        task_status: first_task.map(|task| task.status),
        pr_status: first_task.and_then(|task| task.pr_status.clone()),
        action: String::new(),
        bytes_reclaimed: 0,
    };

    // Secondary gate: never disturb a worktree that may still back a live
    // process, regardless of what the associated task's status says.
    if !run.state.is_terminal() {
        report.action = "skipped:run_not_terminal".to_string();
        return Ok(report);
    }
    if options
        .older_than
        .is_some_and(|cutoff| run.finished_at.unwrap_or(run.created_at) > cutoff)
    {
        report.action = "skipped:too_recent".to_string();
        return Ok(report);
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        OrbitError::Execution(format!(
            "failed to inspect worktree '{}': {error}",
            path.display()
        ))
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        report.action = "skipped:not_a_real_directory".to_string();
        return Ok(report);
    }
    if !is_registered_worktree(repo_root, path)? {
        report.action = "skipped:not_registered_worktree".to_string();
        return Ok(report);
    }

    // Primary gate: only a task settled to rejected, archived, or done
    // licenses deletion. A run's process finishing says nothing about
    // whether the work it produced is settled — a run with no associated
    // task, or one whose task can't be resolved, is retained rather than
    // treated as eligible.
    //
    // Bundle rule (ORB-10427): a worktree that serves several tasks is
    // eligible only when *every* task it serves is settled, so a bundle is
    // never easier to discard than its least-settled member. The first
    // member that blocks deletion becomes the reported task.
    if resolved.is_empty() {
        report.action = "skipped:unattributed".to_string();
        return Ok(report);
    }
    for (task_id, task) in &resolved {
        let Some(task) = task else {
            report.task_id = Some(task_id.clone());
            report.task_status = None;
            report.pr_status = None;
            report.action = "skipped:task_unresolved".to_string();
            return Ok(report);
        };
        if !task_status_permits_deletion(task.status) {
            report.task_id = Some(task_id.clone());
            report.task_status = Some(task.status);
            report.pr_status = task.pr_status.clone();
            report.action = "skipped:task_status_ineligible".to_string();
            return Ok(report);
        }
    }

    // Reported safety net, not a deletion gate: a task can be settled with
    // uncommitted content still sitting in the worktree.
    if !git_output(path, &["status", "--porcelain", "--untracked-files=all"])?
        .trim()
        .is_empty()
    {
        report.action = "skipped:dirty_rescue_candidate".to_string();
        return Ok(report);
    }

    let branch = match branch_name(path) {
        Ok(branch) => branch,
        Err(_) => {
            report.action = "skipped:branch_unknown".to_string();
            return Ok(report);
        }
    };

    let estimated_bytes = directory_bytes(path)?;
    if !options.delete {
        report.action = "would_remove".to_string();
        // In dry-run mode this is the estimate of what `--yes` would reclaim;
        // the result's `dry_run` flag says nothing was actually freed.
        report.bytes_reclaimed = estimated_bytes;
        return Ok(report);
    }

    // Deliberately no `--force`: a last-moment dirtying of the worktree makes
    // Git fail closed. Never replace this with raw recursive deletion.
    remove_worktree(repo_root, path, None, false)?;
    report.bytes_reclaimed = estimated_bytes;
    // The directory is gone and its bytes are reclaimed either way; a branch
    // that cannot be deleted right now (ref lock, checked out elsewhere) is
    // reported rather than turning the whole pass into an error that hides
    // this removal and stops the paths after it.
    report.action = if branch_exists(repo_root, &branch)
        && git_success(repo_root, &["branch", "-D", &branch]).is_err()
    {
        "removed:branch_retained".to_string()
    } else {
        "removed".to_string()
    };
    Ok(report)
}

/// Every directory this run could have left behind.
///
/// The identity is re-derived with the same rule `setup_worktree` used
/// (ORB-10427) — never re-spelled here. A run whose input names no task never
/// reached `setup_worktree`; its worktree, if any, is the shared batch
/// worktree keyed by run id.
fn expected_paths(repo_root: &Path, run: &JobRun) -> Result<Vec<PathBuf>, OrbitError> {
    let input = run.input.as_ref().unwrap_or(&Value::Null);
    let Ok(identity) = WorktreeIdentity::from_input(input, Some(&run.run_id)) else {
        return Ok(vec![resolve_shared_worktree_path(repo_root, &run.run_id)?]);
    };
    let mut paths = vec![identity.path(repo_root)?];
    paths.extend(identity.fallback_path(repo_root)?);
    Ok(paths)
}

/// The tasks a run's worktree serves, empty when the run names none.
fn attributed_task_ids(run: &JobRun) -> Vec<String> {
    WorktreeIdentity::from_input(
        run.input.as_ref().unwrap_or(&Value::Null),
        Some(&run.run_id),
    )
    .map(|identity| identity.task_ids)
    .unwrap_or_default()
}

fn on_disk_worktrees(repo_root: &Path) -> Result<Vec<PathBuf>, OrbitError> {
    let sentinel = resolve_shared_worktree_path(repo_root, "gc-root-sentinel")?;
    let Some(root) = sentinel.parent() else {
        return Ok(Vec::new());
    };
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(root).map_err(|error| {
        OrbitError::Execution(format!(
            "failed to inventory worktree root '{}': {error}",
            root.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            OrbitError::Execution(format!(
                "failed to read an entry under '{}': {error}",
                root.display()
            ))
        })?;
        if entry
            .file_type()
            .map_err(|error| {
                OrbitError::Execution(format!(
                    "failed to inspect '{}': {error}",
                    entry.path().display()
                ))
            })?
            .is_dir()
        {
            paths.push(entry.path());
        }
    }
    Ok(paths)
}

fn branch_name(worktree: &Path) -> Result<String, OrbitError> {
    let branch = git_output(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch.trim();
    if branch.is_empty() || branch == "HEAD" {
        return Err(OrbitError::Execution(format!(
            "cannot safely collect detached worktree '{}'",
            worktree.display()
        )));
    }
    Ok(branch.to_string())
}

/// Match on canonical paths, not raw strings. `git worktree list` reports the
/// resolved path, while the caller holds whatever path it was handed. Where the
/// two differ only by a symlink on the way down — on macOS `/var` and `/tmp`
/// are symlinks into `/private`, so any worktree under them reports one path
/// and is asked about under another — a literal comparison reads a registered
/// worktree as unregistered and GC retains it forever.
///
/// The literal comparison is kept as the fast path, and a registered entry
/// whose directory has already been removed simply fails to canonicalize and
/// does not match, which is the same answer the literal comparison gave.
fn branch_exists(repo_root: &Path, branch: &str) -> bool {
    git_command(
        repo_root,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .status()
    .is_ok_and(|status| status.success())
}

fn directory_bytes(path: &Path) -> Result<u64, OrbitError> {
    let mut total = 0u64;
    let mut pending = vec![path.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).map_err(|error| {
            OrbitError::Execution(format!(
                "failed to measure worktree '{}': {error}",
                current.display()
            ))
        })? {
            let entry = entry.map_err(|error| {
                OrbitError::Execution(format!(
                    "failed to measure an entry under '{}': {error}",
                    current.display()
                ))
            })?;
            let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
                OrbitError::Execution(format!(
                    "failed to measure '{}': {error}",
                    entry.path().display()
                ))
            })?;
            total = total.saturating_add(metadata.len());
            if metadata.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(total)
}

fn git_output(cwd: &Path, args: &[&str]) -> Result<String, OrbitError> {
    let output = git_raw(cwd, args)?;
    String::from_utf8(output.stdout)
        .map_err(|error| OrbitError::Execution(format!("git output was not UTF-8: {error}")))
}

fn git(cwd: &Path, args: &[&str]) -> Result<(), OrbitError> {
    let _ = git_raw(cwd, args)?;
    Ok(())
}

fn git_raw(cwd: &Path, args: &[&str]) -> Result<Output, OrbitError> {
    let output = git_command(cwd, args)
        .output()
        .map_err(|error| OrbitError::Execution(format!("failed to run git: {error}")))?;
    if output.status.success() {
        return Ok(output);
    }
    Err(OrbitError::Execution(format!(
        "git {} failed in '{}': {}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}
