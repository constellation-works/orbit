use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{JobRun, JobRunState};
use serde::Serialize;
use serde_json::Value;

use crate::context::{RuntimeHost, WorktreeGcTaskLookup};

use super::super::git::{git_command_success, git_output, git_success};
use super::cleanup::remove_worktree;
use super::{
    WorktreeIdentity, path_is_registered, registered_worktree_paths, resolve_shared_worktree_path,
};

/// The Cargo build directory a worktree accumulates — the only path
/// target-only collection touches.
const BUILD_OUTPUT_DIR: &str = "target";

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
    /// Walk eligible worktrees to estimate reclaimable bytes. Dry-run skips
    /// the walk unless this is set; deletion always measures before removal.
    pub estimate_bytes: bool,
    /// Reclaim only each eligible worktree's `target/` build output and keep
    /// the checkout. Eligibility needs a terminal run with no live worker,
    /// not a settled task, so failed and blocked runs stay rescuable.
    pub target_only: bool,
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

    let registered = registered_worktree_paths(repo_root)?;
    let lookups = SweepTaskLookups::new(task_host);

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
        let run = selected_runs[0];
        // A single candidate's failure — a git timeout the recovery path
        // could not absorb, a filesystem error, anything else unexpected —
        // must not abort the sweep before it reaches every other worktree.
        // Report it and move on; the pass as a whole still succeeds with a
        // partial summary.
        let report = classify_known(repo_root, path, run, &lookups, options, &registered)
            .unwrap_or_else(|error| {
                tracing::warn!(
                    path = %path.display(),
                    run_id = %run.run_id,
                    %error,
                    "worktree GC failed to classify or remove a worktree; continuing the sweep"
                );
                WorktreeGcReport {
                    path: path.clone(),
                    run_id: Some(run.run_id.clone()),
                    run_state: Some(run.state),
                    task_id: None,
                    task_status: None,
                    pr_status: None,
                    action: format!("failed:{error}"),
                    bytes_reclaimed: 0,
                }
            });
        reports.push(report);
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
    git_success(repo_root, &["worktree", "prune"])?;

    reports.sort_by(|left, right| left.path.cmp(&right.path));
    let bytes_reclaimed = reports.iter().map(|report| report.bytes_reclaimed).sum();
    Ok(WorktreeGcResult {
        dry_run: !options.delete,
        bytes_reclaimed,
        reports,
    })
}

/// One sweep's task lookups. Runs that retry a task, and bundles that share
/// one, ask about each task once. Once a replica's owner proves unreachable,
/// every later lookup in the sweep reports that without waiting on the
/// transport again: the next sweep asks afresh.
struct SweepTaskLookups<'a, H: RuntimeHost + ?Sized> {
    host: &'a H,
    answers: RefCell<BTreeMap<String, WorktreeGcTaskLookup>>,
    owner_unreachable: RefCell<Option<String>>,
}

impl<'a, H: RuntimeHost + ?Sized> SweepTaskLookups<'a, H> {
    fn new(host: &'a H) -> Self {
        Self {
            host,
            answers: RefCell::new(BTreeMap::new()),
            owner_unreachable: RefCell::new(None),
        }
    }

    fn lookup(&self, task_id: &str) -> WorktreeGcTaskLookup {
        if let Some(answer) = self.answers.borrow().get(task_id) {
            return answer.clone();
        }
        if let Some(reason) = self.owner_unreachable.borrow().as_ref() {
            return WorktreeGcTaskLookup::OwnerUnreachable(reason.clone());
        }
        let answer = self.host.lookup_task_for_worktree_gc(task_id);
        match &answer {
            WorktreeGcTaskLookup::OwnerUnreachable(reason) => {
                *self.owner_unreachable.borrow_mut() = Some(reason.clone());
            }
            _ => {
                self.answers
                    .borrow_mut()
                    .insert(task_id.to_string(), answer.clone());
            }
        }
        answer
    }
}

fn classify_known<H: RuntimeHost + ?Sized>(
    repo_root: &Path,
    path: &Path,
    run: &JobRun,
    lookups: &SweepTaskLookups<'_, H>,
    options: &WorktreeGcOptions,
    registered: &BTreeSet<PathBuf>,
) -> Result<WorktreeGcReport, OrbitError> {
    let task_ids = attributed_task_ids(run);
    // Target-only collection never consults task state, so it never pays a
    // store or owner round trip per task.
    let resolved = if options.target_only {
        Vec::new()
    } else {
        task_ids
            .iter()
            .map(|task_id| (task_id.clone(), lookups.lookup(task_id)))
            .collect::<Vec<(String, WorktreeGcTaskLookup)>>()
    };
    let first_task = resolved.first().and_then(|(_, lookup)| match lookup {
        WorktreeGcTaskLookup::Found { status, pr_status } => Some((*status, pr_status.clone())),
        _ => None,
    });

    let mut report = WorktreeGcReport {
        path: path.to_path_buf(),
        run_id: Some(run.run_id.clone()),
        run_state: Some(run.state),
        // A bundle worktree serves several tasks; name all of them until a
        // single one is identified as the reason it is retained.
        task_id: (!task_ids.is_empty()).then(|| task_ids.join(",")),
        task_status: first_task.as_ref().map(|(status, _)| *status),
        pr_status: first_task.and_then(|(_, pr_status)| pr_status),
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
    if !path_is_registered(registered, path) {
        report.action = "skipped:not_registered_worktree".to_string();
        return Ok(report);
    }
    if options.target_only {
        return collect_build_output(path, run, options, report);
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
    for (task_id, lookup) in resolved {
        let (task_status, pr_status, action) = match lookup {
            WorktreeGcTaskLookup::Found { status, pr_status } => {
                if task_status_permits_deletion(status) {
                    continue;
                }
                (Some(status), pr_status, "skipped:task_status_ineligible")
            }
            WorktreeGcTaskLookup::Unresolved => (None, None, "skipped:task_unresolved"),
            // A replica's task state lives on its owner. Not reaching the
            // owner is not evidence the task is unknown, so say which it was.
            WorktreeGcTaskLookup::OwnerUnreachable(reason) => {
                tracing::warn!(
                    path = %path.display(),
                    run_id = %run.run_id,
                    %task_id,
                    %reason,
                    "worktree GC could not reach the workspace owner to resolve a task; retaining the worktree"
                );
                (None, None, "skipped:owner_unreachable")
            }
        };
        report.task_id = Some(task_id);
        report.task_status = task_status;
        report.pr_status = pr_status;
        report.action = action.to_string();
        return Ok(report);
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

    let estimated_bytes = if options.delete || options.estimate_bytes {
        directory_bytes(path)?
    } else {
        0
    };
    if !options.delete {
        report.action = "would_remove".to_string();
        // Dry-run skips the recursive walk unless `estimate_bytes` is set;
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

/// Target-only collection: reclaim `<worktree>/target` and nothing else.
///
/// The checkout — committed, uncommitted and untracked work alike — stays, so
/// a failed or blocked run can still be rescued. That is why the task gate
/// does not apply here: the caller has already required a terminal run, and
/// the build output is reproducible from the checkout it sits in.
fn collect_build_output(
    worktree: &Path,
    run: &JobRun,
    options: &WorktreeGcOptions,
    mut report: WorktreeGcReport,
) -> Result<WorktreeGcReport, OrbitError> {
    // A terminal run record can precede its worker's actual exit (a cancelled
    // agent still finishing a build). A recorded worker that is alive, or
    // whose liveness cannot be decided, keeps its build output.
    if run.pid.is_some_and(|pid| {
        probe_process_liveness(pid, run.pid_start_time.as_deref()) != ProcessLiveness::Exited
    }) {
        report.action = "skipped:worker_alive".to_string();
        return Ok(report);
    }
    let target = worktree.join(BUILD_OUTPUT_DIR);
    let metadata = match fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            report.action = "skipped:no_target".to_string();
            return Ok(report);
        }
        Err(error) => {
            return Err(OrbitError::Execution(format!(
                "failed to inspect build output '{}': {error}",
                target.display()
            )));
        }
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        report.action = "skipped:target_not_a_real_directory".to_string();
        return Ok(report);
    }
    // Only ignored content is build output. A tracked file, or an untracked
    // one Git does not ignore, under `target/` is somebody's work.
    if !git_output(
        worktree,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            BUILD_OUTPUT_DIR,
        ],
    )?
    .trim()
    .is_empty()
    {
        report.action = "skipped:target_not_ignored".to_string();
        return Ok(report);
    }

    let estimated_bytes = if options.delete || options.estimate_bytes {
        directory_bytes(&target)?
    } else {
        0
    };
    report.bytes_reclaimed = estimated_bytes;
    if !options.delete {
        report.action = "would_remove_target".to_string();
        return Ok(report);
    }
    // `remove_dir_all` unlinks symlinks inside the tree rather than following
    // them, so nothing outside `target/` is reachable from here.
    fs::remove_dir_all(&target).map_err(|error| {
        OrbitError::Execution(format!(
            "failed to remove build output '{}': {error}",
            target.display()
        ))
    })?;
    report.action = "removed_target".to_string();
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

fn branch_exists(repo_root: &Path, branch: &str) -> bool {
    git_command_success(
        repo_root,
        &[
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .unwrap_or(false)
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
