use super::super::cleanup::{TRASH_DELETE_TIMEOUT_MS, remove_unregistered_directory};
use super::super::path_is_registered;
use super::paths::directory_bytes;
use super::sweep::{NOT_REGISTERED_REMEDY, WorktreeGcOptions, WorktreeGcReport};
use orbit_common::OrbitError;
use orbit_common::process::identity::{ProcessLiveness, probe_process_liveness, process_is_alive};
use orbit_types::workflow::JobRun;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const LEFTOVER_DETAIL: &str = "Git no longer listed this directory as a worktree: an earlier \
     removal failed partway. Removed the remains of the terminal run's worktree.";

/// Whether a directory is what a failed `git worktree remove` leaves: Git
/// has dropped its administrative entry, so the `.git` link is gone or names
/// a directory that no longer exists. A link that still resolves means the
/// worktree was moved and `git worktree repair` can bring it back, and an
/// unreadable or unexpected `.git` is not something GC can vouch for.
pub(super) fn is_failed_removal_leftover(path: &Path) -> bool {
    let link = path.join(".git");
    match fs::symlink_metadata(&link) {
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        Ok(metadata) if metadata.is_file() => fs::read_to_string(&link)
            .ok()
            .and_then(|content| {
                let target = content.trim().strip_prefix("gitdir:")?.trim();
                (!target.is_empty() && !target.contains(['\n', '\r']))
                    .then(|| PathBuf::from(target))
            })
            .is_some_and(|admin_dir| {
                matches!(
                    fs::symlink_metadata(path.join(admin_dir)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound
                )
            }),
        Ok(_) => false,
    }
}

/// A terminal run record can precede its worker's actual exit. A recorded
/// worker that is alive, or whose liveness cannot be decided, keeps its files.
pub(super) fn worker_may_be_alive(run: &JobRun) -> bool {
    run.pid.is_some_and(|pid| {
        probe_process_liveness(pid, run.pid_start_time.as_deref()) != ProcessLiveness::Exited
    })
}

/// Reclaim the directory a failed removal left behind. The run is terminal
/// and its tasks are settled by the time this runs; Git cannot be asked
/// about the tree (its registration is gone), so the dirty-tree check and the
/// branch are not consulted — Git passed its own checks before it started
/// deleting.
pub(super) fn reclaim_failed_removal_leftover(
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

const TRASH_DETAIL: &str = "A timed-out worktree removal left a relocated trash directory. \
     Removed the leftover directory.";

pub(super) struct TrashDirectoryInfo {
    pid: u32,
    nanos: u128,
}

pub(super) fn parse_trash_directory(file_name: &str) -> Option<TrashDirectoryInfo> {
    let rest = file_name.strip_prefix(".trash-")?;
    let (rest, nanos_str) = rest.rsplit_once('-')?;
    let (name, pid_str) = rest.rsplit_once('-')?;
    if name.is_empty() {
        return None;
    }
    let pid = pid_str.parse::<u32>().ok()?;
    let nanos = nanos_str.parse::<u128>().ok()?;
    Some(TrashDirectoryInfo { pid, nanos })
}

fn is_trash_expired(created_nanos: u128) -> bool {
    let now_nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let age_nanos = now_nanos.saturating_sub(created_nanos);
    let timeout_nanos = (TRASH_DELETE_TIMEOUT_MS as u128) * 1_000_000;
    age_nanos >= timeout_nanos
}

/// A trash name and an expired or dead deleter identify a removal attempt,
/// but do not license deleting a checkout repaired or registered since then.
pub(super) fn reclaim_trash_directory(
    path: &Path,
    trash: &TrashDirectoryInfo,
    registered: &BTreeSet<PathBuf>,
    options: &WorktreeGcOptions,
) -> Result<WorktreeGcReport, OrbitError> {
    let mut report = WorktreeGcReport {
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
    };
    if path_is_registered(registered, path) {
        report.action = "skipped:registered_worktree".to_string();
        report.detail = Some(
            "Git still registers this directory as a worktree; its trash name does not authorize removal."
                .to_string(),
        );
        return Ok(report);
    }
    if !is_failed_removal_leftover(path) {
        report.action = "skipped:not_registered_worktree".to_string();
        report.detail = Some(NOT_REGISTERED_REMEDY.to_string());
        return Ok(report);
    }
    if process_is_alive(trash.pid) && !is_trash_expired(trash.nanos) {
        report.action = "skipped:deleter_alive".to_string();
        return Ok(report);
    }
    report.bytes_reclaimed = if options.delete || options.estimate_bytes {
        directory_bytes(path)?
    } else {
        0
    };
    report.detail = Some(TRASH_DETAIL.to_string());
    if !options.delete {
        report.action = "would_remove".to_string();
        return Ok(report);
    }
    remove_unregistered_directory(path)?;
    report.action = "removed".to_string();
    Ok(report)
}
