mod classify;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod held;
// Native Linux and macOS collection uses held no-follow directories.
// Other platforms retain configured host output.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod host_targets;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod host_targets {
    use super::sweep::{WorktreeGcOptions, WorktreeGcReport};
    use orbit_types::workflow::JobRun;
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    pub(super) fn run_has_host_target(_repo_root: &Path, _run: &JobRun) -> bool {
        false
    }

    pub(super) fn collect_host_targets(
        _repo_root: &Path,
        _runs: &[JobRun],
        _options: &WorktreeGcOptions,
        _removed_worktrees: &BTreeSet<PathBuf>,
    ) -> Vec<WorktreeGcReport> {
        Vec::new()
    }
}
mod leftovers;
mod paths;
mod reclaim;
mod rescue;
mod sweep;

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    feature = "test-support"
))]
pub use host_targets::with_host_target_removal_hook;
pub use paths::run_worktree_paths;
pub use reclaim::run_worktree_has_reclaim_output;
// Keep the report type reachable at the existing GC module path.
#[allow(unused_imports)]
pub use sweep::WorktreeGcReport;
pub use sweep::{WorktreeGcOptions, WorktreeGcResult, collect_worktrees};
