use super::system::human_bytes;
use super::*;

pub(super) fn doctor_check_worktree_reclaim(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    const CHECK: &str = "worktree-reclaim";
    if !runtime.paths().repo_root.join(".git").exists() {
        return check(
            CHECK,
            WorkspaceDoctorStatus::Skipped,
            "not a git checkout".into(),
        );
    }
    match runtime.reclaimable_worktrees() {
        Ok(result) if result.bytes_reclaimed > 10 * 1024 * 1024 * 1024 => actionable_check(
            CHECK, WorkspaceDoctorStatus::Warning,
            format!("{} reclaimable in kept run worktrees", human_bytes(result.bytes_reclaimed)),
            "Inspect `orbit gc worktrees --reclaim`; run `ORBIT_OPERATOR=1 orbit gc worktrees --reclaim --confirm` to reclaim declared output.".into(),
        ),
        Ok(result) => check(CHECK, WorkspaceDoctorStatus::Ok,
            format!("{} reclaimable in kept run worktrees", human_bytes(result.bytes_reclaimed))),
        Err(error) => check(CHECK, WorkspaceDoctorStatus::Warning,
            format!("cannot inspect kept worktree output: {error}")),
    }
}

/// The plugin section's source-built rows: each plugin this host built from
/// source at install time, with its command, consent, profile and artifact
/// digest, and any drift `orbit plugin doctor` finds in those builds.
pub(super) fn doctor_check_plugin_builds(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let rows = match runtime.plugin_build_doctor() {
        Ok(rows) => rows,
        Err(error) => {
            return check(
                "plugin-builds",
                WorkspaceDoctorStatus::Warning,
                format!("cannot inspect source-built plugins: {error}"),
            );
        }
    };
    if rows.is_empty() {
        return check(
            "plugin-builds",
            WorkspaceDoctorStatus::Skipped,
            "no plugin on this host was built from source".to_string(),
        );
    }
    let findings = rows.iter().filter(|row| !row.intentional).count();
    let message = rows
        .iter()
        .map(|row| format!("{}: {}", row.plugin, row.message))
        .collect::<Vec<_>>()
        .join("\n");
    if findings == 0 {
        return check("plugin-builds", WorkspaceDoctorStatus::Ok, message);
    }
    actionable_check(
        "plugin-builds",
        WorkspaceDoctorStatus::Warning,
        message,
        "Take the step each source-built plugin row names, then rerun `orbit plugin doctor`."
            .to_string(),
    )
}

/// A workspace registered for PR delivery whose Git remotes name no network
/// host would fail every shipped task at `pr_open`; admission refuses those
/// tasks with the same verdict this row reports.
pub(super) fn doctor_check_forge_remote(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    const CHECK: &str = "forge-remote";
    if runtime.automatic_delivery_ship_mode() != orbit_core::ShipMode::Pr {
        return check(
            CHECK,
            WorkspaceDoctorStatus::Skipped,
            "workspace is not registered for PR delivery; no forge remote needed".to_string(),
        );
    }
    match runtime.pr_forge_refusal() {
        None => check(
            CHECK,
            WorkspaceDoctorStatus::Ok,
            "PR delivery has a Git remote on a network host".to_string(),
        ),
        Some(refusal) => actionable_check(
            CHECK,
            WorkspaceDoctorStatus::Warning,
            refusal.to_string(),
            "Run `orbit workspace ship-mode local` to deliver locally, or add a Git remote on \
             your forge host. A single task can ship locally with the \
             `delivery:task_local_pipeline` tag."
                .to_string(),
        ),
    }
}

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

    // `ls-files` is outside `run_git`'s admitted subcommands.
    let mut command = std::process::Command::new("git");
    command
        .args(["ls-files", "--", ".orbit"])
        .current_dir(repo_root)
        .env("GIT_OPTIONAL_LOCKS", "0");
    let output = orbit_common::process::run_bounded_capped(
        &mut command,
        orbit_common::fs::git::GIT_LOCAL_TIMEOUT,
        orbit_common::fs::git::GIT_OUTPUT_LIMIT,
    );

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
        Err(error) => check(
            "tracked-orbit-files",
            WorkspaceDoctorStatus::Skipped,
            format!("git ls-files could not run: {error}"),
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
        Ok(config) => {
            let ignored = config.ignored_crew_properties.iter().map(|ignored| {
                actionable_check(
                    "config",
                    WorkspaceDoctorStatus::Warning,
                    ignored.warning_message(),
                    ignored.remediation(),
                )
            });
            // A lane pointed at a disabled crew loads fine but refuses every
            // dispatch on that lane; say so before a run finds out.
            let disabled_lanes = config.disabled_lane_crews().into_iter().map(|lane| {
                actionable_check(
                    "config",
                    WorkspaceDoctorStatus::Warning,
                    lane.warning_message(),
                    lane.remediation(),
                )
            });
            let warnings = ignored.chain(disabled_lanes).collect::<Vec<_>>();
            if warnings.is_empty() {
                vec![check(
                    "config",
                    WorkspaceDoctorStatus::Ok,
                    format!("valid ({})", path.display()),
                )]
            } else {
                warnings
            }
        }
        Err(error) => vec![check(
            "config",
            WorkspaceDoctorStatus::Error,
            format!("invalid ({}): {error}", path.display()),
        )],
    }
}

/// Cheap database/header and schema-ledger checks; full page integrity is opt-in.
pub(super) fn doctor_check_database(runtime: &OrbitRuntime, deep: bool) -> WorkspaceDoctorResult {
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
    if deep && let Err(error) = store.quick_check() {
        return check(
            "database",
            WorkspaceDoctorStatus::Error,
            format!("integrity check failed: {error}"),
        );
    }
    let probe = if deep {
        "quick_check ok"
    } else {
        "database readable (integrity scan: orbit doctor --deep)"
    };
    match store.schema_version() {
        Ok(version) if version == SUPPORTED_SCHEMA_VERSION => check(
            "database",
            WorkspaceDoctorStatus::Ok,
            format!("{probe}; schema version {version} matches this binary"),
        ),
        Ok(version) if version < SUPPORTED_SCHEMA_VERSION => check(
            "database",
            WorkspaceDoctorStatus::Warning,
            format!(
                "{probe}; schema version {version} is behind this binary \
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
            format!("{probe}; cannot read migration ledger: {error}"),
        ),
    }
}

/// Free space on the volume holding the workspace `.orbit` directory.
pub(super) fn doctor_check_disk_space(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let root = runtime.local_root();
    disk_space_check(&root)
}

/// Tasks stored in this workspace, counted from the validated task index
/// without hydrating any bundle.
fn stored_task_count(runtime: &OrbitRuntime) -> Result<usize, orbit_common::OrbitError> {
    Ok(runtime
        .task_candidates(&orbit_core::application::task::TaskListFilter::default(), 0)?
        .total)
}

/// Cheap chunk coverage check against the authoritative task store.
pub(super) fn doctor_check_search_index(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    match runtime
        .search_index_stats()
        .and_then(|stats| Ok((stats, stored_task_count(runtime)?)))
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
                "{} lock file(s) with dead holder records: {}",
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

/// What store retention could reclaim now under `retention.audit_days` and
/// `retention.runs_days`. Measured without the blob reference scan, so the
/// blob figure is an upper bound; `orbit gc audit` reports the exact one.
pub(super) fn doctor_check_store_retention(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    const CHECK: &str = "store-retention";
    let overview = match runtime.store_retention_overview() {
        Ok(overview) => overview,
        Err(error) => {
            return check(
                CHECK,
                WorkspaceDoctorStatus::Warning,
                format!("cannot measure reclaimable store space: {error}"),
            );
        }
    };
    check(
        CHECK,
        WorkspaceDoctorStatus::Ok,
        format!(
            "reclaimable: audit {} rows ({}) older than {} days; run state of {} terminal runs \
             ({}) older than {} days; up to {} of {} audit blobs; store file {} with {} free. \
             Plan with `orbit gc audit` and `orbit gc runs`, apply with `--apply`",
            overview.audit_rows,
            human_bytes(overview.audit_bytes),
            overview.audit_days,
            overview.run_states,
            human_bytes(overview.run_state_bytes),
            overview.runs_days,
            human_bytes(overview.blob_bytes_past_cutoff),
            human_bytes(overview.blob_bytes),
            human_bytes(overview.store.file_bytes),
            human_bytes(overview.store.freelist_bytes),
        ),
    )
}
