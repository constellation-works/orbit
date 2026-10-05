use super::*;

/// Free/total space thresholds for the volume containing `path`.
pub(crate) fn disk_space_check(path: &Path) -> WorkspaceDoctorResult {
    let (available, total) = match (fs2::available_space(path), fs2::total_space(path)) {
        (Ok(available), Ok(total)) => (available, total),
        (Err(error), _) | (_, Err(error)) => {
            return check(
                "disk-space",
                WorkspaceDoctorStatus::Warning,
                format!(
                    "cannot determine free space for {}: {error}",
                    path.display()
                ),
            );
        }
    };
    let pct_free = if total == 0 {
        100.0
    } else {
        available as f64 * 100.0 / total as f64
    };
    let message = format!(
        "{} free of {} ({pct_free:.1}%) on the volume holding {}",
        human_bytes(available),
        human_bytes(total),
        path.display()
    );
    let status = if available < DISK_FAIL_BYTES || pct_free < DISK_FAIL_PCT {
        WorkspaceDoctorStatus::Error
    } else if available < DISK_WARN_BYTES || pct_free < DISK_WARN_PCT {
        WorkspaceDoctorStatus::Warning
    } else {
        WorkspaceDoctorStatus::Ok
    };
    check("disk-space", status, message)
}

/// Lock files in the workspace-local state directory.
///
/// Task bundle locks live in the registry-owned global bundle store, which is
/// not a workspace-local diagnostic target.
pub(crate) fn collect_lock_files(paths: &WorkspacePaths) -> Vec<PathBuf> {
    let mut lock_files = Vec::new();
    let Ok(entries) = std::fs::read_dir(&paths.state_dir) else {
        return lock_files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_lock = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".lock"));
        if is_lock && path.is_file() {
            lock_files.push(path);
        }
    }
    lock_files
}

/// Liveness probe for a recorded holder PID. `kill(pid, 0)` — EPERM still
/// means alive. Conservative on doubt: an unprobeable PID counts as alive so
/// a live holder is never reported stale.
#[cfg(unix)]
pub(crate) fn process_is_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true;
    };
    if pid <= 0 {
        return false;
    }
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Non-Unix: no cheap probe; treat every holder as alive (never report stale).
#[cfg(not(unix))]
pub(crate) fn process_is_alive(_pid: u32) -> bool {
    true
}

pub(super) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
