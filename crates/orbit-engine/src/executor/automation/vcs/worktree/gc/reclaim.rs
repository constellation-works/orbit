use super::leftovers::worker_may_be_alive;
use super::paths::run_worktree_paths;
use super::sweep::{WorktreeGcOptions, WorktreeGcReport};
use orbit_common::OrbitError;
use orbit_types::workflow::JobRun;
use std::path::Path;

/// Reclaim only declared output, keeping committed and unmatched work.
pub(super) fn collect_build_output(
    worktree: &Path,
    run: &JobRun,
    options: &WorktreeGcOptions,
    mut report: WorktreeGcReport,
) -> Result<WorktreeGcReport, OrbitError> {
    if worker_may_be_alive(run) {
        report.action = "skipped:worker_alive".to_string();
        return Ok(report);
    }
    report.reclaim =
        super::super::reclaim::collect(worktree, reclaim_patterns(options), options.delete)?;
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

pub(super) fn reclaim_patterns(options: &WorktreeGcOptions) -> &[String] {
    // The same default as config admission for hosts without configuration.
    static DEFAULT: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| {
        ["target", ".orbit/tmp/*-target", ".orbit/tmp/base-wt"]
            .map(String::from)
            .to_vec()
    });
    options.reclaim_patterns.as_deref().unwrap_or(&DEFAULT)
}

/// Whether any of this run's checkouts contains a declared output match, or
/// has host validation output. A prefilter only: collection still enforces
/// registration, worker, containment and Git gates.
pub fn run_worktree_has_reclaim_output(
    repo_root: &Path,
    run: &JobRun,
    patterns: &[String],
) -> bool {
    run_worktree_paths(repo_root, run).is_ok_and(|paths| {
        paths
            .iter()
            .any(|path| super::super::reclaim::has_matches(path, patterns))
    }) || super::host_targets::run_has_host_target(repo_root, run)
}
