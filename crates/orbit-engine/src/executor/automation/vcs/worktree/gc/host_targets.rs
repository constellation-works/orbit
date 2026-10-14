//! Host validation output of run checkouts.
//!
//! On Linux required validation builds into a host-owned target outside the
//! checkout, keyed by the checkout's canonical path
//! (`required_command::host_validation_target`).
//! Removing the checkout leaves that target behind. GC attributes a target to
//! runs only by re-deriving each recorded run's worktree path with the same
//! key rule; a directory name no run record derives is never inferred to
//! belong to anyone and is retained. Detached scratch checkouts (base reruns,
//! host evidence, recovery) are keyed by paths no run record names, so their
//! targets stay as `skipped:unattributed_host_target`.

use super::super::super::required_command::{
    HOST_VALIDATION_TARGETS_DIR, host_validation_target_key,
};
use super::super::cleanup::remove_unregistered_directory;
use super::leftovers::worker_may_be_alive;
use super::paths::{attributed_task_ids, directory_bytes, retained_bytes, run_worktree_paths};
use super::reclaim::reclaim_patterns;
use super::sweep::{WorktreeGcOptions, WorktreeGcReport};
use orbit_common::OrbitError;
use orbit_types::workflow::JobRun;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const UNATTRIBUTED_DETAIL: &str = "No recorded run's checkout derives this host validation \
     target; it may belong to a base rerun, host-evidence or recovery checkout. GC retains it.";

/// The runs whose checkout one host target was built for.
struct Attribution<'a> {
    /// The run's worktree path, as the sweep reports it.
    worktree: PathBuf,
    /// The canonical checkout path the target's name hashes.
    source: PathBuf,
    runs: Vec<&'a JobRun>,
}

/// The host validation targets directory of `repo_root`, when it is a
/// primary checkout and the directory exists. Every component must be a real
/// directory: a redirect is refused rather than followed.
fn targets_root(repo_root: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let Ok(primary) = fs::canonicalize(repo_root) else {
        return Ok(None);
    };
    // Targets live under the primary checkout that owns the Git common
    // directory; a linked-worktree workspace's runs build under another root.
    if !fs::symlink_metadata(primary.join(".git")).is_ok_and(|meta| meta.is_dir()) {
        return Ok(None);
    }
    let mut root = primary;
    for component in HOST_VALIDATION_TARGETS_DIR {
        root.push(component);
        match fs::symlink_metadata(&root) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(OrbitError::PolicyDenied(format!(
                    "host validation target component '{}' must be a directory, not a redirect",
                    root.display()
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Some(root))
}

/// The canonical checkout path a run's worktree had, derived without the
/// worktree itself existing: the managed worktree root resolves, the
/// worktree's own name is one plain component.
fn canonical_source(worktree: &Path) -> Option<PathBuf> {
    let parent = fs::canonicalize(worktree.parent()?).ok()?;
    Some(parent.join(worktree.file_name()?))
}

fn attributions<'a>(repo_root: &Path, runs: &'a [JobRun]) -> BTreeMap<String, Attribution<'a>> {
    let mut index = BTreeMap::<String, Attribution<'a>>::new();
    for run in runs {
        for worktree in run_worktree_paths(repo_root, run).unwrap_or_default() {
            let Some(source) = canonical_source(&worktree) else {
                continue;
            };
            index
                .entry(host_validation_target_key(&source))
                .or_insert_with(|| Attribution {
                    worktree,
                    source,
                    runs: Vec::new(),
                })
                .runs
                .push(run);
        }
    }
    index
}

/// Whether a run's checkout has a host validation target. A prefilter only:
/// collection still applies every run, worker and containment gate.
pub(super) fn run_has_host_target(repo_root: &Path, run: &JobRun) -> bool {
    let Ok(Some(root)) = targets_root(repo_root) else {
        return false;
    };
    run_worktree_paths(repo_root, run)
        .unwrap_or_default()
        .iter()
        .filter_map(|worktree| canonical_source(worktree))
        .any(|source| {
            fs::symlink_metadata(root.join(host_validation_target_key(&source)))
                .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink())
        })
}

/// Classify, and with `options.delete` remove, the host validation targets
/// of recorded runs. `removed_worktrees` are the checkouts this sweep removed
/// or would remove. A failure is reported, never returned, so the rest of the
/// sweep's figures survive it.
pub(super) fn collect_host_targets(
    repo_root: &Path,
    runs: &[JobRun],
    options: &WorktreeGcOptions,
    removed_worktrees: &BTreeSet<PathBuf>,
) -> Vec<WorktreeGcReport> {
    let root = match targets_root(repo_root) {
        Ok(Some(root)) => root,
        Ok(None) => return Vec::new(),
        Err(error) => {
            let mut report = blank_report(
                repo_root
                    .join(HOST_VALIDATION_TARGETS_DIR.iter().collect::<PathBuf>())
                    .as_path(),
            );
            report.action = format!("failed:{error}");
            return vec![report];
        }
    };
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) => {
            let mut report = blank_report(&root);
            report.action = format!("failed:inventory host validation targets: {error}");
            return vec![report];
        }
    };
    let index = attributions(repo_root, runs);
    let mut reports = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                let mut report = blank_report(&root);
                report.action = format!("failed:read a host validation target: {error}");
                reports.push(report);
                continue;
            }
        };
        let path = entry.path();
        let attribution = entry.file_name().to_str().and_then(|key| index.get(key));
        let Some(attribution) = attribution else {
            if options.run_id.is_none() {
                let mut report = blank_report(&path);
                report.action = "skipped:unattributed_host_target".to_string();
                report.detail = Some(UNATTRIBUTED_DETAIL.to_string());
                report.retained_bytes = retained_bytes(&path, options);
                reports.push(report);
            }
            continue;
        };
        let selected = match options.run_id.as_deref() {
            Some(wanted) => match attribution.runs.iter().find(|run| run.run_id == wanted) {
                Some(run) => *run,
                None => continue,
            },
            None => attribution.runs[0],
        };
        let mut report = blank_report(&path);
        report.run_id = Some(selected.run_id.clone());
        report.run_state = Some(selected.state);
        let task_ids = attributed_task_ids(selected);
        report.task_id = (!task_ids.is_empty()).then(|| task_ids.join(","));
        report.detail = Some(format!(
            "host validation output of checkout '{}'",
            attribution.source.display()
        ));
        if let Err(error) = classify(
            repo_root,
            &root,
            &path,
            attribution,
            options,
            removed_worktrees,
            &mut report,
        ) {
            tracing::warn!(path = %path.display(), %error,
                "worktree GC failed to reclaim a host validation target; continuing the sweep");
            report.bytes_reclaimed = 0;
            report.action = format!("failed:{error}");
        }
        reports.push(report);
    }
    reports
}

/// Decide `report`'s action, removing the target when the sweep deletes.
fn classify(
    repo_root: &Path,
    root: &Path,
    path: &Path,
    attribution: &Attribution<'_>,
    options: &WorktreeGcOptions,
    removed_worktrees: &BTreeSet<PathBuf>,
    report: &mut WorktreeGcReport,
) -> Result<(), OrbitError> {
    let Ok(observed) = fs::symlink_metadata(path) else {
        report.action = "skipped:not_a_real_directory".to_string();
        return Ok(());
    };
    if !observed.is_dir() || observed.file_type().is_symlink() {
        report.action = "skipped:not_a_real_directory".to_string();
        return Ok(());
    }
    // Every run whose checkout derives this target must be finished: a
    // shared path stays live while any run mapping it is active.
    if let Some(run) = attribution.runs.iter().find(|run| !run.state.is_terminal()) {
        report.run_id = Some(run.run_id.clone());
        report.run_state = Some(run.state);
        report.action = "skipped:run_not_terminal".to_string();
        return Ok(());
    }
    if let Some(run) = attribution.runs.iter().find(|run| worker_may_be_alive(run)) {
        report.run_id = Some(run.run_id.clone());
        report.run_state = Some(run.state);
        report.action = "skipped:worker_alive".to_string();
        return Ok(());
    }
    // Like declared output in a kept worktree, a sweep that reclaims kept
    // output is not held back by the whole-worktree age floor.
    if !options.reclaim_kept
        && options.older_than.is_some_and(|cutoff| {
            attribution
                .runs
                .iter()
                .any(|run| run.finished_at.unwrap_or(run.created_at) > cutoff)
        })
    {
        report.action = "skipped:too_recent".to_string();
        report.retained_bytes = retained_bytes(path, options);
        return Ok(());
    }
    // Output of a checkout that still exists is reclaimed with the
    // checkout's declared output, and never when that is disabled.
    let source_present = fs::symlink_metadata(&attribution.source).is_ok();
    if source_present
        && !removed_worktrees.contains(&attribution.worktree)
        && (!(options.target_only || options.reclaim_kept) || reclaim_patterns(options).is_empty())
    {
        report.action = "skipped:source_retained".to_string();
        report.retained_bytes = retained_bytes(path, options);
        return Ok(());
    }
    let measure = options.delete || options.estimate_bytes || options.target_only;
    if measure {
        report.bytes_reclaimed = directory_bytes(path)?;
    }
    if !options.delete {
        report.action = "would_reclaim".to_string();
        return Ok(());
    }
    // Re-establish containment immediately before deletion: the same real
    // root, and the same directory that was classified, never a substitute.
    let unchanged = targets_root(repo_root)
        .ok()
        .flatten()
        .is_some_and(|current| current == root)
        && fs::symlink_metadata(path).is_ok_and(|current| same_directory(&observed, &current));
    if !unchanged {
        report.bytes_reclaimed = 0;
        report.action = "skipped:not_a_real_directory".to_string();
        return Ok(());
    }
    remove_unregistered_directory(path)?;
    tracing::info!(path = %path.display(), bytes = report.bytes_reclaimed,
        "reclaimed a terminal run's host validation output");
    report.action = "reclaimed".to_string();
    Ok(())
}

#[cfg(unix)]
fn same_directory(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    after.is_dir()
        && !after.file_type().is_symlink()
        && before.dev() == after.dev()
        && before.ino() == after.ino()
}

#[cfg(not(unix))]
fn same_directory(_before: &fs::Metadata, after: &fs::Metadata) -> bool {
    after.is_dir() && !after.file_type().is_symlink()
}

fn blank_report(path: &Path) -> WorktreeGcReport {
    WorktreeGcReport {
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
