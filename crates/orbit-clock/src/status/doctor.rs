//! Clock activity and sweep log freshness checks for doctor.

use std::path::Path;

use crate::settings::{LAUNCHD_LABEL, SYSTEMD_UNIT};

use super::ClockStatus;

/// Why `orbit doctor`'s `clock-unit` row must not be ok.
///
/// `orbit clock status` keeps an operator pause healthy. Doctor asks the
/// narrower question the sweep timer's enabled and active state answers: on
/// Linux the systemd user timer must be enabled and active, and on macOS the
/// launchd agent must be loaded and `logs/sweep.log` must have been written
/// within the configured tick interval plus one tick deadline. The text names
/// `orbit clock enable`.
///
/// A disabled systemd timer that is still active is left to the existing
/// health issue, whose fix is `orbit clock pause`.
pub fn doctor_clock_not_running_issue(status: &ClockStatus, global_root: &Path) -> Option<String> {
    match status.platform {
        "systemd" => systemd_doctor_activity_issue(status),
        "launchd" => launchd_doctor_activity_issue(status, global_root),
        _ => None,
    }
}

fn systemd_doctor_activity_issue(status: &ClockStatus) -> Option<String> {
    let detail = match (status.enabled, status.running) {
        (true, Some(true)) | (false, Some(true)) => return None,
        (false, Some(false)) => "disabled and inactive",
        (false, None) => "disabled",
        (true, _) => "inactive",
    };
    Some(format!(
        "systemd user timer {SYSTEMD_UNIT}.timer is {detail}; run `orbit clock enable` to enable it and verify it is active"
    ))
}

fn launchd_doctor_activity_issue(status: &ClockStatus, global_root: &Path) -> Option<String> {
    let mut parts = Vec::new();
    if !status.enabled || !status.loaded {
        parts.push(format!("launchd agent {LAUNCHD_LABEL} is not loaded"));
    }
    if let Some(issue) = sweep_log_tick_issue(global_root, status.configured_cadence_seconds) {
        parts.push(issue);
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "{}; run `orbit clock enable` to re-arm the sweep clock",
        parts.join("; ")
    ))
}

/// `None` when the sweep log was written within the tick interval and deadline.
fn sweep_log_tick_issue(global_root: &Path, cadence_seconds: u64) -> Option<String> {
    let path = crate::install::sweep_log_path(global_root);
    let displayed = path.display().to_string();
    let freshness_limit =
        std::time::Duration::from_secs(cadence_seconds).saturating_add(crate::TICK_DEADLINE);
    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return match most_recent_rotated_sweep_log_mtime(&path) {
                Ok(Some(modified))
                    if std::time::SystemTime::now()
                        .duration_since(modified)
                        .unwrap_or_default()
                        <= freshness_limit =>
                {
                    None
                }
                Ok(Some(_)) => Some(format!(
                    "{displayed} was rotated and has not been recreated within the {cadence_seconds}s tick interval plus the {}s tick deadline",
                    crate::TICK_DEADLINE.as_secs()
                )),
                Ok(None) => Some(format!("{displayed} has not been written")),
                Err(error) => Some(format!(
                    "could not inspect rotated logs for {displayed}: {error}"
                )),
            };
        }
        Err(error) => return Some(format!("could not read {displayed}: {error}")),
    };
    if !meta.is_file() {
        return Some(format!("{displayed} is not a regular sweep log"));
    }
    let modified = match meta.modified() {
        Ok(modified) => modified,
        Err(error) => return Some(format!("could not read {displayed} mtime: {error}")),
    };
    let age = std::time::SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default();
    if age > freshness_limit {
        Some(format!(
            "{displayed} is older than the {cadence_seconds}s tick interval plus the {}s tick deadline",
            crate::TICK_DEADLINE.as_secs()
        ))
    } else {
        None
    }
}

/// Find the newest retained sweep-log archive, used only while rotation has
/// temporarily removed the active path.
fn most_recent_rotated_sweep_log_mtime(
    path: &Path,
) -> Result<Option<std::time::SystemTime>, std::io::Error> {
    let Some(parent) = path.parent() else {
        return Ok(None);
    };
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(None);
    };
    let archive_prefix = format!("{file_name}.");
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut newest = None;
    for entry in entries {
        let entry = entry?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(&archive_prefix)
        {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.is_file() {
            let modified = metadata.modified()?;
            newest = Some(newest.map_or(modified, |previous: std::time::SystemTime| {
                previous.max(modified)
            }));
        }
    }
    Ok(newest)
}
