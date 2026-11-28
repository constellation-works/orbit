use std::path::{Path, PathBuf};

use orbit_common::fs::selector::anchor_path;
use serde_json::Value;

use super::WorktreeBoundaryGuard;
use super::fingerprint::{GitWorktreeFingerprint, git_stdout_bytes, nul_paths};

impl WorktreeBoundaryGuard {
    /// Compute overlaps against the admitted selectors and the candidate's
    /// committed and dirty paths. An unreadable diff cannot prove disjointness.
    pub(super) fn claimed_primary_move_conflicts(
        &self,
        process_env: &orbit_common::Env,
        primary_after: &GitWorktreeFingerprint,
        primary_dirt_paths: &[String],
    ) -> Option<Vec<String>> {
        let task_paths = self.claimed_paths.as_ref()?;
        if self.primary_before.head == primary_after.head {
            return None;
        }
        let base = self.primary_baseline.base_tip.as_deref()?;
        let candidate_paths = revision_paths(
            process_env,
            &self.assigned_root,
            &format!("{base}...{}", self.assigned_before.head),
        )?;
        let mut primary_paths = revision_paths(
            process_env,
            &self.primary_root,
            &format!("{}..{}", self.primary_before.head, primary_after.head),
        )?;
        primary_paths.extend_from_slice(primary_dirt_paths);
        let assigned_paths = task_paths.iter().map(PathBuf::as_path).chain(
            self.assigned_before
                .dirty_paths
                .iter()
                .chain(&candidate_paths)
                .map(Path::new),
        );
        let assigned_paths = assigned_paths.collect::<Vec<_>>();
        primary_paths.retain(|path| {
            let primary_path = Path::new(path);
            assigned_paths.iter().any(|assigned_path| {
                *assigned_path == Path::new(".")
                    || primary_path.starts_with(assigned_path)
                    || assigned_path.starts_with(primary_path)
            })
        });
        Some(primary_paths)
    }
}

pub(super) fn task_paths(task_ctx: Option<&Value>) -> Vec<PathBuf> {
    task_ctx
        .and_then(|task| task.get("context_files"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|selector| anchor_path(selector).ok())
        .collect()
}

fn revision_paths(
    process_env: &orbit_common::Env,
    root: &Path,
    range: &str,
) -> Option<Vec<String>> {
    let bytes = git_stdout_bytes(
        process_env,
        root,
        &[
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            "--ignore-submodules=none",
            range,
            "--",
        ],
    )
    .ok()?;
    Some(nul_paths(&bytes))
}
