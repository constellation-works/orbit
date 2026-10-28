use super::super::super::git::git_success;
use super::super::{path_is_registered, registered_worktree_paths, rescue};
use super::classify::{SweepTaskLookups, classify_known};
use super::host_targets::collect_host_targets;
use super::leftovers::{parse_trash_directory, reclaim_trash_directory, worker_may_be_alive};
use super::paths::{
    WorktreeRootListing, on_disk_worktrees, retained_bytes, run_worktree_paths,
    unresolvable_run_report,
};
use super::reclaim::reclaim_patterns;
use super::workers::{map_with_sweep_workers, sweep_scan_workers};
use crate::context::RuntimeHost;
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{JobRun, JobRunState};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;

/// Task statuses that settle the work as done — the only statuses that
/// license discarding a run's worktree and branch outright. A task parked in
/// `someday` licenses it only under the parked-age rule
/// (`WorktreeGcOptions::parked_older_than`). Every other status (including
/// `blocked` and `review`) retains it, and an unresolvable or missing task id
/// retains it as well: the collector fails closed.
pub(super) fn task_status_permits_deletion(status: TaskStatus) -> bool {
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
    /// Walk eligible worktrees to estimate reclaimable bytes. A full
    /// collection's dry run skips the walk unless this is set; deletion always
    /// measures before removal, and a target-only sweep always measures the
    /// declared output and host validation targets it reports.
    pub estimate_bytes: bool,
    /// Reclaim declared rebuildable output and keep the checkout. Eligibility
    /// needs a terminal run with no live worker, not a settled task, so failed
    /// and blocked runs stay rescuable.
    pub target_only: bool,
    /// Declared relative paths; omission keeps the historical target default.
    pub reclaim_patterns: Option<Vec<String>>,
    /// Reclaim paths in worktrees retained by the full sweep.
    pub reclaim_kept: bool,
    /// Remove a settled run's dirty worktree once its uncommitted candidate is
    /// recoverable elsewhere (a carry ref or published branch holding exactly
    /// it, or a verified rescue patch). Without it a dirty worktree is kept.
    pub rescue_dirty: bool,
    /// A task parked in `someday` since at least this instant no longer
    /// retains its run's worktree. The worktree goes only when the run
    /// branch's commits survive it: reachable from a ref that is not a run
    /// branch, or on `origin`. `None` keeps every parked worktree.
    pub parked_older_than: Option<DateTime<Utc>>,
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
    pub reclaim: Vec<super::super::reclaim::WorktreeReclaimReport>,
    /// Where a removed dirty worktree's uncommitted candidate can be recovered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<super::super::rescue::WorktreeGcRecovery>,
    /// What a kept worktree still occupies after this sweep, measured when the
    /// sweep deletes or estimates; absent for a live run's worktree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_bytes: Option<u64>,
}

impl WorktreeGcReport {
    /// An empty report at `path`: no run or task attributed, no action yet,
    /// nothing reclaimed, measured or retained.
    pub(super) fn blank(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            run_id: None,
            run_state: None,
            task_id: None,
            task_status: None,
            pr_status: None,
            action: String::new(),
            bytes_reclaimed: 0,
            detail: None,
            reclaim: Vec::new(),
            recovery: None,
            retained_bytes: None,
        }
    }

    /// `blank` at `path`, attributed to `run`.
    pub(super) fn for_run(path: &Path, run: &JobRun) -> Self {
        Self {
            run_id: Some(run.run_id.clone()),
            run_state: Some(run.state),
            ..Self::blank(path)
        }
    }
}

/// What an operator can do about a directory Git does not list as a
/// worktree of this checkout and that GC cannot show to be the remains of a
/// failed removal. GC never removes one.
pub(super) const NOT_REGISTERED_REMEDY: &str = "Git does not list this directory as a worktree of this \
     checkout, so GC never removes it. If the worktree was moved, `git worktree repair <path>` \
     re-registers it; otherwise inspect it and delete it by hand once nothing in it is needed.";

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
    // Path derivation runs before the run-id filter, so one record whose
    // token cannot name a directory used to abort every sweep — including
    // one scoped to an unrelated run — until that record was pruned
    // [ORB-14099]. Keep the runs that resolve, so a shared path is still
    // ambiguous when another resolvable run occupies it, and report an
    // in-scope failure instead of returning it. Host validation targets are
    // attributed from the same derivation.
    let mut reports = Vec::new();
    let mut run_paths = Vec::with_capacity(runs.len());
    for run in runs {
        match run_worktree_paths(repo_root, run) {
            Ok(paths) => run_paths.push((run, paths)),
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
    // Only a directory on disk is classified, so index only the derived paths
    // the worktree root lists; every run deriving one still joins its entry.
    // Without a listing every derived path is indexed.
    let listed = WorktreeRootListing::read(repo_root);
    let mut known_paths = BTreeMap::<PathBuf, Vec<&JobRun>>::new();
    for (run, paths) in &run_paths {
        for path in paths {
            if listed.as_ref().is_none_or(|listed| listed.contains(path)) {
                known_paths.entry(path.clone()).or_default().push(*run);
            }
        }
    }

    let registered = registered_worktree_paths(repo_root)?;
    let candidates = known_paths
        .iter()
        .filter_map(|(path, matching_runs)| {
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
            (!selected_runs.is_empty() && path.exists()).then_some((
                path.as_path(),
                matching_runs.as_slice(),
                selected_runs,
            ))
        })
        .collect::<Vec<_>>();
    // Target-only estimates read each candidate independently and ask no
    // owner about tasks; every other sweep keeps one worker, so its owner
    // lookups stay memoized across the sweep and its removals stay in order.
    // Such an estimate removes no checkout either, so its host validation
    // targets are measured alongside instead of after.
    let read_only_estimate = options.target_only && !options.delete;
    let (classified, host_targets) = thread::scope(|scope| {
        let host_targets = read_only_estimate
            .then(|| {
                thread::Builder::new()
                    .name("orbit-gc-host-targets".into())
                    .spawn_scoped(scope, || {
                        collect_host_targets(repo_root, &run_paths, options, &BTreeSet::new())
                    })
                    .ok()
            })
            .flatten();
        let classified = map_with_sweep_workers(
            &candidates,
            sweep_scan_workers(read_only_estimate),
            || SweepTaskLookups::new(task_host),
            |lookups, (path, matching_runs, selected_runs)| {
                classify_candidate(
                    repo_root,
                    path,
                    matching_runs,
                    selected_runs,
                    lookups,
                    options,
                    &registered,
                )
            },
        );
        let host_targets = host_targets.map(|handle| {
            handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        });
        (classified, host_targets)
    });
    reports.extend(classified.into_iter().flatten());

    let host_targets = host_targets.unwrap_or_else(|| {
        // A checkout this sweep removes (or would) no longer needs its host
        // validation output; one already gone leaves that output orphaned.
        let removed_worktrees = reports
            .iter()
            .filter(|report| {
                report.action.starts_with("removed") || report.action == "would_remove"
            })
            .map(|report| report.path.clone())
            .collect::<BTreeSet<_>>();
        collect_host_targets(repo_root, &run_paths, options, &removed_worktrees)
    });
    reports.extend(host_targets);

    if options.run_id.is_none() {
        let known: BTreeSet<_> = known_paths.keys().cloned().collect();
        for entry in on_disk_worktrees(repo_root)? {
            if !known.contains(&entry) {
                let file_name = entry.file_name().and_then(|val| val.to_str());
                let trash = if !options.target_only {
                    file_name.and_then(parse_trash_directory)
                } else {
                    None
                };
                if let Some(trash) = trash {
                    let report = reclaim_trash_directory(&entry, &trash, &registered, options)
                        .unwrap_or_else(|error| {
                            tracing::warn!(
                                path = %entry.display(),
                                %error,
                                "failed to reclaim trash directory"
                            );
                            WorktreeGcReport {
                                action: format!("failed:{error}"),
                                ..WorktreeGcReport::blank(&entry)
                            }
                        });
                    reports.push(report);
                } else {
                    reports.push(WorktreeGcReport {
                        action: "skipped:unrecognized".to_string(),
                        ..WorktreeGcReport::blank(&entry)
                    });
                }
            }
        }
    }

    // Estimates (including doctor's probe) are read-only.
    if options.delete {
        git_success(repo_root, &["worktree", "prune"])?;
        // A patch the operator removed no longer needs its base commit.
        rescue::release_orphan_rescue_bases(repo_root)?;
    }

    reports.sort_by(|left, right| left.path.cmp(&right.path));
    let bytes_reclaimed = reports.iter().map(|report| report.bytes_reclaimed).sum();
    Ok(WorktreeGcResult {
        dry_run: !options.delete,
        bytes_reclaimed,
        reports,
    })
}

/// Classify one mapped path, and reclaim or measure it as the sweep asks.
/// A path several runs map is reported once per selected run when one of
/// them is still live.
fn classify_candidate<H: RuntimeHost + ?Sized>(
    repo_root: &Path,
    path: &Path,
    matching_runs: &[&JobRun],
    selected_runs: &[&JobRun],
    lookups: &SweepTaskLookups<'_, H>,
    options: &WorktreeGcOptions,
    registered: &BTreeSet<PathBuf>,
) -> Vec<WorktreeGcReport> {
    if matching_runs.len() > 1 && matching_runs.iter().any(|run| !run.state.is_terminal()) {
        return selected_runs
            .iter()
            .map(|run| WorktreeGcReport {
                action: "skipped:ambiguous_run_path".to_string(),
                ..WorktreeGcReport::for_run(path, run)
            })
            .collect();
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
                classify_known(repo_root, path, other, lookups, &preflight, registered)?;
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
        classify_known(repo_root, path, run, lookups, options, registered)
    })()
    .unwrap_or_else(|error| {
        tracing::warn!(
            path = %path.display(),
            run_id = %run.run_id,
            %error,
            "worktree GC failed to classify or remove a worktree; continuing the sweep"
        );
        WorktreeGcReport {
            action: format!("failed:{error}"),
            ..WorktreeGcReport::for_run(path, run)
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
        && path_is_registered(registered, path)
        && fs::symlink_metadata(path)
            .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink())
    {
        match super::super::reclaim::collect(path, reclaim_patterns(options), options.delete) {
            Ok(paths) => {
                report.bytes_reclaimed = paths.iter().map(|path| path.bytes_reclaimed).sum();
                report.reclaim = paths;
            }
            Err(error) => {
                report.detail = Some(format!("reclaim failed: {error}"));
            }
        }
    }
    // A kept worktree's size makes a leak visible in the sweep's own
    // report. A live run's checkout is never walked.
    if report.action.starts_with("skipped:")
        && matching_runs.iter().all(|run| run.state.is_terminal())
    {
        report.retained_bytes = retained_bytes(path, options);
    }
    vec![report]
}
