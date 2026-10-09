use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness};
use orbit_types::task::{TaskStatus, task_id_prefix};
use orbit_types::workflow::{JobRun, JobRunState};
use serde::Serialize;
use serde_json::Value;

use crate::context::{RuntimeHost, WorktreeGcTaskLookup};

use super::super::git::{git_command_success, git_output, git_success};
use super::cleanup::{remove_unregistered_directory, remove_worktree};
use super::{
    WorktreeIdentity, path_is_registered, registered_worktree_paths, resolve_shared_worktree_path,
};

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
    /// Reclaim declared rebuildable output and keep the checkout. Eligibility
    /// needs a terminal run with no live worker, not a settled task, so failed
    /// and blocked runs stay rescuable.
    pub target_only: bool,
    /// Declared relative paths; omission keeps the historical target default.
    pub reclaim_patterns: Option<Vec<String>>,
    /// Reclaim paths in worktrees retained by the full sweep.
    pub reclaim_kept: bool,
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
    /// Why the action was taken, when the action alone does not say: the
    /// owner transport's error, the missing owner route, the settled claim
    /// that licensed removal, or the remedy for a worktree GC cannot touch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Per-pattern output retained, measured or reclaimed inside this worktree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reclaim: Vec<super::reclaim::WorktreeReclaimReport>,
}

/// What an operator can do about a directory Git does not list as a
/// worktree of this checkout and that GC cannot show to be the remains of a
/// failed removal. GC never removes one.
const NOT_REGISTERED_REMEDY: &str = "Git does not list this directory as a worktree of this \
     checkout, so GC never removes it. If the worktree was moved, `git worktree repair <path>` \
     re-registers it; otherwise inspect it and delete it by hand once nothing in it is needed.";

const LEFTOVER_DETAIL: &str = "Git no longer listed this directory as a worktree: an earlier \
     removal failed partway. Removed the remains of the terminal run's worktree.";

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
    // Path derivation runs before the run-id filter, so one record whose
    // token cannot name a directory used to abort every sweep — including
    // one scoped to an unrelated run — until that record was pruned
    // [ORB-14099]. Index the runs that resolve, so a shared path is still
    // ambiguous when another resolvable run occupies it, and report an
    // in-scope failure instead of returning it.
    let mut reports = Vec::new();
    for run in runs {
        match run_worktree_paths(repo_root, run) {
            Ok(paths) => {
                for path in paths {
                    known_paths.entry(path).or_default().push(run);
                }
            }
            Err(error) => {
                tracing::warn!(
                    run_id = %run.run_id,
                    %error,
                    "worktree GC could not derive a worktree path for a run; continuing the sweep"
                );
                if options
                    .run_id
                    .as_deref()
                    .is_none_or(|wanted| wanted == run.run_id)
                {
                    reports.push(unresolvable_run_report(run, &error));
                }
            }
        }
    }

    let registered = registered_worktree_paths(repo_root)?;
    let lookups = SweepTaskLookups::new(task_host);

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
        if matching_runs.len() > 1 && matching_runs.iter().any(|run| !run.state.is_terminal()) {
            reports.extend(selected_runs.into_iter().map(|run| WorktreeGcReport {
                path: path.clone(),
                run_id: Some(run.run_id.clone()),
                run_state: Some(run.state),
                task_id: None,
                task_status: None,
                pr_status: None,
                action: "skipped:ambiguous_run_path".to_string(),
                bytes_reclaimed: 0,
                detail: None,
                reclaim: Vec::new(),
            }));
            continue;
        }
        let run = selected_runs[0];
        // A single candidate's failure — a git timeout the recovery path
        // could not absorb, a filesystem error, anything else unexpected —
        // must not abort the sweep before it reaches every other worktree.
        // Report it and move on; the pass as a whole still succeeds with a
        // partial summary.
        let report = (|| {
            // Terminal overlap is collectable only when every mapped run
            // passes the same safety gates. Preflight without deleting so an
            // unsettled task or live worker belonging to another run still
            // protects the shared path. Remove it at most once per sweep.
            let preflight = WorktreeGcOptions {
                delete: false,
                estimate_bytes: false,
                ..options.clone()
            };
            for other in matching_runs
                .iter()
                .filter(|other| other.run_id != run.run_id)
            {
                let mut report =
                    classify_known(repo_root, path, other, &lookups, &preflight, &registered)?;
                let eligible = if options.target_only {
                    "would_reclaim"
                } else {
                    "would_remove"
                };
                if report.action != eligible
                    && !(options.target_only && report.action == "skipped:no_reclaimable_paths")
                {
                    report.detail = Some(format!(
                        "another mapped run retains this path: {}",
                        report.action
                    ));
                    report.run_id = Some(run.run_id.clone());
                    report.run_state = Some(run.state);
                    report.action = "skipped:ambiguous_run_path".to_string();
                    return Ok(report);
                }
            }
            classify_known(repo_root, path, run, &lookups, options, &registered)
        })()
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
                detail: None,
                reclaim: Vec::new(),
            }
        });
        let mut report = report;
        // Full collection preserves the checkout's original keep reason and
        // reclaims only declared output after every mapped run passes the
        // terminal/worker/registration gates. Never reclaim a shared active path.
        if options.reclaim_kept
            && !options.target_only
            && report.action.starts_with("skipped:")
            && matching_runs
                .iter()
                .all(|run| run.state.is_terminal() && !worker_may_be_alive(run))
            && path_is_registered(&registered, path)
            && fs::symlink_metadata(path)
                .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink())
        {
            match super::reclaim::collect(path, reclaim_patterns(options), options.delete) {
                Ok(paths) => {
                    report.bytes_reclaimed = paths.iter().map(|path| path.bytes_reclaimed).sum();
                    report.reclaim = paths;
                }
                Err(error) => {
                    report.detail = Some(format!("reclaim failed: {error}"));
                }
            }
        }
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
                    detail: None,
                    reclaim: Vec::new(),
                });
            }
        }
    }

    // Estimates (including doctor's probe) are read-only.
    if options.delete {
        git_success(repo_root, &["worktree", "prune"])?;
    }

    reports.sort_by(|left, right| left.path.cmp(&right.path));
    let bytes_reclaimed = reports.iter().map(|report| report.bytes_reclaimed).sum();
    Ok(WorktreeGcResult {
        dry_run: !options.delete,
        bytes_reclaimed,
        reports,
    })
}

/// One sweep's task lookups. Results and transport failures are memoized by
/// owner route and task, so a down owner is contacted once per prefix while
/// local and unroutable prefixes still get their own verdict. A missing route
/// is not an outage and is not carried over: another run's claim may name a route.
struct SweepTaskLookups<'a, H: RuntimeHost + ?Sized> {
    host: &'a H,
    answers: RefCell<BTreeMap<(String, String), WorktreeGcTaskLookup>>,
    owner_unreachable: RefCell<BTreeMap<String, String>>,
}

impl<'a, H: RuntimeHost + ?Sized> SweepTaskLookups<'a, H> {
    fn new(host: &'a H) -> Self {
        Self {
            host,
            answers: RefCell::new(BTreeMap::new()),
            owner_unreachable: RefCell::new(BTreeMap::new()),
        }
    }

    fn lookup(&self, run_id: &str, task_id: &str) -> WorktreeGcTaskLookup {
        let scope = self
            .host
            .worktree_gc_task_lookup_scope(run_id)
            .map(|scope| format!("{scope}/{}", task_id_prefix(task_id).unwrap_or_default()));
        if let Some(scope) = scope.as_ref() {
            let key = (scope.clone(), task_id.to_string());
            if let Some(answer) = self.answers.borrow().get(&key) {
                return answer.clone();
            }
            if let Some(reason) = self.owner_unreachable.borrow().get(scope) {
                return WorktreeGcTaskLookup::OwnerUnreachable(reason.clone());
            }
        }
        let answer = self.host.lookup_task_for_worktree_gc(run_id, task_id);
        if let Some(scope) = scope {
            match &answer {
                WorktreeGcTaskLookup::OwnerUnreachable(reason) => {
                    self.owner_unreachable
                        .borrow_mut()
                        .insert(scope, reason.clone());
                }
                WorktreeGcTaskLookup::NoOwnerRoute(_) => {}
                _ => {
                    self.answers
                        .borrow_mut()
                        .insert((scope, task_id.to_string()), answer.clone());
                }
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
    // A claimed leaf whose claim is settled with its owner needs no task
    // answer: the owner already holds what the leaf delivered.
    let settled_claim = if options.target_only {
        None
    } else {
        lookups.host.settled_claim_for_worktree_gc(&run.run_id)
    };
    // Target-only collection never consults task state, so it never pays a
    // store or owner round trip per task; neither does a settled claim.
    let resolved = if options.target_only || settled_claim.is_some() {
        Vec::new()
    } else {
        task_ids
            .iter()
            .map(|task_id| (task_id.clone(), lookups.lookup(&run.run_id, task_id)))
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
        detail: None,
        reclaim: Vec::new(),
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
    // Git unregisters a worktree before it finishes deleting the directory,
    // so a removal that failed partway leaves a mapped, unregistered
    // directory. That leftover is reclaimed below through the same task
    // gates; any other unregistered directory may be a moved worktree.
    let registered_here = path_is_registered(registered, path);
    if !registered_here && (options.target_only || !is_failed_removal_leftover(path)) {
        report.action = "skipped:not_registered_worktree".to_string();
        report.detail = Some(NOT_REGISTERED_REMEDY.to_string());
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
    //
    // A claimed leaf's settled claim stands in for its task's status: the
    // follower holds no task records, and the owner already has the leaf's
    // delivery whatever the task's status says now.
    if let Some(settlement) = settled_claim {
        report.detail = Some(settlement);
    } else if resolved.is_empty() {
        report.action = "skipped:unattributed".to_string();
        return Ok(report);
    }
    for (task_id, lookup) in resolved {
        let (task_status, pr_status, action, detail) = match lookup {
            WorktreeGcTaskLookup::Found { status, pr_status } => {
                if task_status_permits_deletion(status) {
                    continue;
                }
                (
                    Some(status),
                    pr_status,
                    "skipped:task_status_ineligible",
                    None,
                )
            }
            WorktreeGcTaskLookup::Unresolved => (None, None, "skipped:task_unresolved", None),
            WorktreeGcTaskLookup::TaskPrefixUnroutable => {
                (None, None, "skipped:task_prefix_unroutable", None)
            }
            WorktreeGcTaskLookup::NoOwnerRoute(reason) => {
                (None, None, "skipped:no_owner_route", Some(reason))
            }
            WorktreeGcTaskLookup::OwnerLookupFailed(reason) => {
                (None, None, "skipped:owner_lookup_failed", Some(reason))
            }
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
                (None, None, "skipped:owner_unreachable", Some(reason))
            }
        };
        report.task_id = Some(task_id);
        report.task_status = task_status;
        report.pr_status = pr_status;
        report.action = action.to_string();
        report.detail = detail;
        return Ok(report);
    }

    if !registered_here {
        return reclaim_failed_removal_leftover(path, run, options, report);
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

/// Whether a directory is what a failed `git worktree remove` leaves: Git
/// has dropped its administrative entry, so the `.git` link is gone or names
/// a directory that no longer exists. A link that still resolves means the
/// worktree was moved and `git worktree repair` can bring it back, and an
/// unreadable or unexpected `.git` is not something GC can vouch for.
fn is_failed_removal_leftover(path: &Path) -> bool {
    let link = path.join(".git");
    match fs::symlink_metadata(&link) {
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        Ok(metadata) if metadata.is_file() => fs::read_to_string(&link)
            .ok()
            .and_then(|content| {
                content
                    .lines()
                    .find_map(|line| line.strip_prefix("gitdir:"))
                    .map(|target| PathBuf::from(target.trim()))
            })
            .is_some_and(|admin_dir| {
                matches!(
                    fs::symlink_metadata(&admin_dir),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound
                )
            }),
        Ok(_) => false,
    }
}

/// A terminal run record can precede its worker's actual exit. A recorded
/// worker that is alive, or whose liveness cannot be decided, keeps its files.
fn worker_may_be_alive(run: &JobRun) -> bool {
    run.pid.is_some_and(|pid| {
        probe_process_liveness(pid, run.pid_start_time.as_deref()) != ProcessLiveness::Exited
    })
}

/// Reclaim the directory a failed removal left behind. The run is terminal
/// and its tasks are settled by the time this runs; Git cannot be asked
/// about the tree (its registration is gone), so the dirty-tree check and the
/// branch are not consulted — Git passed its own checks before it started
/// deleting.
fn reclaim_failed_removal_leftover(
    path: &Path,
    run: &JobRun,
    options: &WorktreeGcOptions,
    mut report: WorktreeGcReport,
) -> Result<WorktreeGcReport, OrbitError> {
    if worker_may_be_alive(run) {
        report.action = "skipped:worker_alive".to_string();
        return Ok(report);
    }
    let estimated_bytes = if options.delete || options.estimate_bytes {
        directory_bytes(path)?
    } else {
        0
    };
    report.bytes_reclaimed = estimated_bytes;
    if !options.delete {
        report.action = "would_remove".to_string();
        report.detail = Some(LEFTOVER_DETAIL.to_string());
        return Ok(report);
    }
    remove_unregistered_directory(path)?;
    report.action = "removed".to_string();
    report.detail = Some(LEFTOVER_DETAIL.to_string());
    Ok(report)
}

/// Reclaim only declared output, keeping committed and unmatched work.
fn collect_build_output(
    worktree: &Path,
    run: &JobRun,
    options: &WorktreeGcOptions,
    mut report: WorktreeGcReport,
) -> Result<WorktreeGcReport, OrbitError> {
    if worker_may_be_alive(run) {
        report.action = "skipped:worker_alive".to_string();
        return Ok(report);
    }
    report.reclaim = super::reclaim::collect(worktree, reclaim_patterns(options), options.delete)?;
    report.bytes_reclaimed = report.reclaim.iter().map(|path| path.bytes_reclaimed).sum();
    report.action = if report
        .reclaim
        .iter()
        .any(|path| matches!(path.action.as_str(), "removed" | "would_remove"))
    {
        if options.delete {
            "reclaimed"
        } else {
            "would_reclaim"
        }
    } else {
        "skipped:no_reclaimable_paths"
    }
    .to_string();
    Ok(report)
}

fn reclaim_patterns(options: &WorktreeGcOptions) -> &[String] {
    // The same default as config admission for hosts without configuration.
    static DEFAULT: std::sync::LazyLock<Vec<String>> =
        std::sync::LazyLock::new(|| vec!["target".into()]);
    options.reclaim_patterns.as_deref().unwrap_or(&DEFAULT)
}

/// Whether any of this run's checkouts contains a declared output match.
/// A prefilter only: collection still enforces registration, worker and Git gates.
pub fn run_worktree_has_reclaim_output(
    repo_root: &Path,
    run: &JobRun,
    patterns: &[String],
) -> bool {
    run_worktree_paths(repo_root, run).is_ok_and(|paths| {
        paths
            .iter()
            .any(|path| super::reclaim::has_matches(path, patterns))
    })
}

/// A run whose stored token cannot name a directory. Setup never created one
/// for it, so the report has no path; the sweep continues and the entry stays
/// visible until the record is pruned.
fn unresolvable_run_report(run: &JobRun, error: &OrbitError) -> WorktreeGcReport {
    let task_ids = attributed_task_ids(run);
    WorktreeGcReport {
        path: PathBuf::new(),
        run_id: Some(run.run_id.clone()),
        run_state: Some(run.state),
        task_id: (!task_ids.is_empty()).then(|| task_ids.join(",")),
        task_status: None,
        pr_status: None,
        action: format!("failed:{error}"),
        bytes_reclaimed: 0,
        detail: None,
        reclaim: Vec::new(),
    }
}

/// Every directory this run could have left behind, primary path first.
///
/// The identity is re-derived with the same rule `setup_worktree` used
/// (ORB-10427) — never re-spelled here. A run whose input names no task never
/// reached `setup_worktree`; its worktree, if any, is the shared batch
/// worktree keyed by run id.
pub fn run_worktree_paths(repo_root: &Path, run: &JobRun) -> Result<Vec<PathBuf>, OrbitError> {
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
