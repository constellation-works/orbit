use super::*;

/// Warn when git still tracks files under `.orbit/`. Sync rewrites the
/// managed ignore block but never runs git; the operator untracks once.
pub(super) fn doctor_check_tracked_orbit_files(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let repo_root = &runtime.paths().repo_root;
    if !repo_root.join(".git").exists() {
        return check(
            "tracked-orbit-files",
            WorkspaceDoctorStatus::Skipped,
            "not a git checkout".to_string(),
        );
    }

    let output = std::process::Command::new("git")
        .args(["ls-files", "--", ".orbit"])
        .current_dir(repo_root)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let tracked = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| !line.is_empty())
                .count();
            if tracked == 0 {
                check(
                    "tracked-orbit-files",
                    WorkspaceDoctorStatus::Ok,
                    "no tracked files under .orbit/".to_string(),
                )
            } else {
                actionable_check(
                    "tracked-orbit-files",
                    WorkspaceDoctorStatus::Warning,
                    format!(
                        "{tracked} tracked file(s) under .orbit/; .orbit/ is per-user state and should not be in git"
                    ),
                    "git rm -r --cached .orbit".to_string(),
                )
            }
        }
        Ok(output) => {
            let detail = String::from_utf8_lossy(&output.stderr);
            let detail = detail.trim();
            check(
                "tracked-orbit-files",
                WorkspaceDoctorStatus::Skipped,
                if detail.is_empty() {
                    "git ls-files failed".to_string()
                } else {
                    format!("git ls-files failed: {detail}")
                },
            )
        }
        Err(_) => check(
            "tracked-orbit-files",
            WorkspaceDoctorStatus::Skipped,
            "git is not available".to_string(),
        ),
    }
}

/// Parse + validate the effective (workspace-over-global) `config.toml`.
pub(super) fn doctor_check_config(runtime: &OrbitRuntime) -> Vec<WorkspaceDoctorResult> {
    let path = match runtime.config_path() {
        Ok(path) => path,
        Err(error) => {
            return vec![check(
                "config",
                WorkspaceDoctorStatus::Error,
                format!("cannot select effective config: {error}"),
            )];
        }
    };
    match orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::new(
        runtime.global_root(),
        runtime.data_root(),
    )) {
        Ok(config) if config.ignored_crew_properties.is_empty() => vec![check(
            "config",
            WorkspaceDoctorStatus::Ok,
            format!("valid ({})", path.display()),
        )],
        Ok(config) => config
            .ignored_crew_properties
            .iter()
            .map(|ignored| {
                actionable_check(
                    "config",
                    WorkspaceDoctorStatus::Warning,
                    ignored.warning_message(),
                    ignored.remediation(),
                )
            })
            .collect(),
        Err(error) => vec![check(
            "config",
            WorkspaceDoctorStatus::Error,
            format!("invalid ({}): {error}", path.display()),
        )],
    }
}

/// `PRAGMA quick_check` plus migration-ledger schema version vs binary.
pub(super) fn doctor_check_database(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let store = match runtime.sqlite_store_for_diagnostics() {
        Ok(store) => store,
        Err(error) => {
            return check(
                "database",
                WorkspaceDoctorStatus::Error,
                format!("cannot open store database: {error}"),
            );
        }
    };
    if let Err(error) = store.quick_check() {
        return check(
            "database",
            WorkspaceDoctorStatus::Error,
            format!("integrity check failed: {error}"),
        );
    }
    match store.schema_version() {
        Ok(version) if version == SUPPORTED_SCHEMA_VERSION => check(
            "database",
            WorkspaceDoctorStatus::Ok,
            format!("quick_check ok; schema version {version} matches this binary"),
        ),
        Ok(version) if version < SUPPORTED_SCHEMA_VERSION => check(
            "database",
            WorkspaceDoctorStatus::Warning,
            format!(
                "quick_check ok; schema version {version} is behind this binary \
                 ({SUPPORTED_SCHEMA_VERSION}) — migrations apply on next store open"
            ),
        ),
        Ok(version) => check(
            "database",
            WorkspaceDoctorStatus::Error,
            format!(
                "schema version {version} is newer than this binary supports \
                 ({SUPPORTED_SCHEMA_VERSION}); upgrade orbit"
            ),
        ),
        Err(error) => check(
            "database",
            WorkspaceDoctorStatus::Warning,
            format!("quick_check ok; cannot read migration ledger: {error}"),
        ),
    }
}

/// Free space on the volume holding the workspace `.orbit` directory.
pub(super) fn doctor_check_disk_space(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let root = runtime.local_root();
    disk_space_check(&root)
}

/// Cheap chunk coverage check against the authoritative task store.
pub(super) fn doctor_check_search_index(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    match runtime
        .search_index_stats()
        .and_then(|stats| Ok((stats, runtime.list_tasks()?.len())))
    {
        Err(error) => check(
            "search-index",
            WorkspaceDoctorStatus::Warning,
            format!("cannot read search index: {error}"),
        ),
        Ok((stats, tasks)) => {
            let detail = format!(
                "{} chunks, {} indexed tasks / {tasks} stored tasks",
                stats.chunks, stats.tasks
            );
            if stats.tasks != tasks {
                actionable_check(
                    "search-index",
                    WorkspaceDoctorStatus::Warning,
                    detail,
                    "Run `orbit search reindex`, then rerun `orbit doctor`.".to_string(),
                )
            } else {
                check("search-index", WorkspaceDoctorStatus::Ok, detail)
            }
        }
    }
}

pub(super) fn doctor_check_stale_locks(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let lock_files = collect_lock_files(runtime.paths());
    let mut stale = Vec::new();
    for path in &lock_files {
        let Some(holder) = orbit_store::read_lock_holder(path) else {
            continue;
        };
        if !process_is_alive(holder.pid) {
            stale.push(format!(
                "{} (dead pid {}, op: {}, since {})",
                path.display(),
                holder.pid,
                holder.label,
                holder.acquired_at
            ));
        }
    }
    if stale.is_empty() {
        check(
            "stale-locks",
            WorkspaceDoctorStatus::Ok,
            format!("{} lock file(s) scanned, none stale", lock_files.len()),
        )
    } else {
        actionable_check(
            "stale-locks",
            WorkspaceDoctorStatus::Warning,
            format!(
                "{} lock file(s) left by dead holders (the OS already released the \
                 flock; safe to delete): {}",
                stale.len(),
                stale.join("; ")
            ),
            "Run `orbit doctor --fix-stale-locks`.".to_string(),
        )
    }
}

/// Active task reservations are diagnosed separately from filesystem lock
/// files. The runtime classifier deliberately ignores fresh/live/ambiguous
/// reservations and reports only owner/task states that prove inactivity.
pub(super) fn doctor_check_task_reservations(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let stale = match runtime.list_stale_task_reservations() {
        Ok(stale) => stale,
        Err(error) => {
            return actionable_check(
                "task-reservations",
                WorkspaceDoctorStatus::Warning,
                format!("cannot inspect active task reservations: {error}"),
                "Resolve the store/runtime error, then rerun `orbit doctor`.".to_string(),
            );
        }
    };
    if stale.is_empty() {
        return check(
            "task-reservations",
            WorkspaceDoctorStatus::Ok,
            "no conclusively stale active task reservations".to_string(),
        );
    }
    let detail = stale
        .iter()
        .map(|reservation| {
            let tasks = if reservation.task_ids.is_empty() {
                "no associated tasks".to_string()
            } else {
                format!("tasks {}", reservation.task_ids.join(", "))
            };
            let owner = reservation
                .owner_run_id
                .as_deref()
                .map_or_else(|| "unowned".to_string(), |run_id| format!("run {run_id}"));
            format!(
                "{} ({tasks}, {owner}): {}",
                reservation.reservation_id, reservation.reason
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    actionable_check(
        "task-reservations",
        WorkspaceDoctorStatus::Warning,
        format!(
            "{} conclusively stale active task reservation(s): {detail}",
            stale.len()
        ),
        "Run `orbit doctor --fix-stale-task-locks`.".to_string(),
    )
}
