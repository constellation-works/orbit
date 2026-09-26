use super::*;

/// Task-store partitions under `<global_root>/tasks/workspaces/<ws_id>/`
/// that no live registry claim covers — left behind by a `workspace teardown`
/// run on an older binary, or by deleting a checkout without running teardown
/// [ORB-12109]. Scoped to the whole host, not just this workspace, because the
/// partition directory is itself host-global.
///
/// A partition is normally named for its *task-registry* workspace id, so the
/// task registry claims it while the bound checkout's `orbit_dir` exists.
/// `orbit workspace init` may use the catalog's `ws_*` id directly; that
/// catalog claim is live only while its recorded checkout is present. The
/// synthetic `--root` partition is the other permanent claimant. Comparing
/// the directory name against catalog `ws_*` ids alone reported every
/// `<slug>-<hash>` partition — including live ones — as orphaned [ORB-12119].
///
/// Partitions that still hold task bundles are reported without inviting a
/// deletion unless their checkout is confirmed gone. A lost or rebuilt
/// registry leaves every other checkout's live partition looking like
/// abandoned residue, and the recovery for that is `orbit task reindex` in the
/// owning checkout [ORB-12131]; a checkout that merely failed to stat may be
/// intact behind an unmounted volume or an unreadable parent directory, and
/// the recovery is to restore access [ORB-12143].
pub(super) fn doctor_check_orphan_task_stores(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let global_root = runtime.global_root();
    let partitions = match task_store::inspect_task_store_partitions(&global_root) {
        Ok(Some(partitions)) => partitions,
        Ok(None) => {
            return check(
                "orphan-task-stores",
                WorkspaceDoctorStatus::Skipped,
                "no task-store partitions on this host yet".to_string(),
            );
        }
        Err(error) => {
            return check(
                "orphan-task-stores",
                WorkspaceDoctorStatus::Warning,
                format!("cannot resolve task-store partition owners: {error}"),
            );
        }
    };

    // Each category gets its own clause, so one row can describe a host that
    // has several at once, and its own remediation step in reporting order.
    let mut clauses: Vec<String> = Vec::new();
    let mut steps: Vec<String> = Vec::new();

    if !partitions.unowned.is_empty() {
        clauses.push(format!(
            "{} task-store partition(s) hold task bundles that no workspace binding claims: {}",
            partitions.unowned.len(),
            describe_partitions(&partitions.unowned)
        ));
        steps.push(
            "Run `orbit task reindex` from each checkout that owns those bundles to rebind them."
                .to_string(),
        );
    }
    if !partitions.unreachable.is_empty() {
        clauses.push(format!(
            "{} populated partition(s) whose bound checkout could not be reached: {}",
            partitions.unreachable.len(),
            describe_unreachable_partitions(&partitions.unreachable)
        ));
        steps.push(
            "Restore access to the unreachable checkouts (remount the volume, repair directory \
             permissions) and re-run `orbit doctor`; a populated partition is never deleted while \
             its checkout cannot be stat-ed."
                .to_string(),
        );
    }
    if !partitions.stale.is_empty() {
        clauses.push(format!(
            "{} stale task-store partition(s) point at missing checkout directories: {}",
            partitions.stale.len(),
            describe_partitions(&partitions.stale)
        ));
    }
    if !partitions.removable.is_empty() {
        clauses.push(format!(
            "{} orphaned empty task-store partition(s) (no workspace binding claims them): {}",
            partitions.removable.len(),
            describe_partitions(&partitions.removable)
        ));
    }

    if clauses.is_empty() {
        return check(
            "orphan-task-stores",
            WorkspaceDoctorStatus::Ok,
            format!(
                "{} task-store partition(s) scanned, all claimed by a workspace binding",
                partitions.scanned
            ),
        );
    }

    if !partitions.stale.is_empty() || !partitions.removable.is_empty() {
        // Every entry in `stale` is populated by construction (empty
        // partitions land in `removable` instead), so its deletion always
        // destroys task bundles; the remediation must say so up front rather
        // than let the operator discover it from the repair's own report.
        let bundle_note = if partitions.stale.is_empty() {
            ""
        } else {
            " Populated stale partitions are deleted along with their task bundles."
        };
        steps.push(if steps.is_empty() {
            format!("Run `orbit doctor --fix-orphan-task-stores --confirm`.{bundle_note}")
        } else {
            format!(
                "Then run `orbit doctor --fix-orphan-task-stores --confirm`, which removes only \
                 the empty partitions and the partitions whose checkout is confirmed \
                 gone.{bundle_note}"
            )
        });
    }

    actionable_check(
        "orphan-task-stores",
        WorkspaceDoctorStatus::Warning,
        clauses.join("; "),
        steps.join(" "),
    )
}

/// Blocked tasks whose run failed because dispatch could not find the provider
/// launcher. Nothing re-evaluates such a block on its own, so a launcher
/// installed since leaves the task stranded; this row says which blocks no
/// longer reproduce and names the command that requeues them. Blocks caused
/// by the task's own work are not listed.
pub(super) fn doctor_check_infra_blocked_tasks(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let infra_blocked = match runtime.infra_blocked_tasks() {
        Ok(infra_blocked) => infra_blocked,
        Err(error) => {
            return check(
                "infra-blocked-tasks",
                WorkspaceDoctorStatus::Warning,
                format!("cannot classify blocked tasks: {error}"),
            );
        }
    };
    if infra_blocked.is_empty() {
        return check(
            "infra-blocked-tasks",
            WorkspaceDoctorStatus::Ok,
            "no task is blocked by a missing provider launcher".to_string(),
        );
    }

    let (cleared, reproducing): (Vec<_>, Vec<_>) = infra_blocked
        .iter()
        .partition(|blocked| blocked.launcher.is_some());
    let mut clauses = Vec::new();
    let mut steps = Vec::new();
    if !cleared.is_empty() {
        clauses.push(format!(
            "{} task(s) blocked by a missing provider launcher that now resolves: {}",
            cleared.len(),
            cleared
                .iter()
                .map(|blocked| format!(
                    "{} (`{}` at {})",
                    blocked.task_id,
                    blocked.program,
                    blocked
                        .launcher
                        .as_deref()
                        .map_or_else(String::new, |path| path.display().to_string())
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        steps.push(
            "Run `orbit task recheck-blocked --confirm` to return the cleared tasks to backlog."
                .to_string(),
        );
    }
    if !reproducing.is_empty() {
        clauses.push(format!(
            "{} task(s) blocked by a provider launcher that is still missing: {}",
            reproducing.len(),
            reproducing
                .iter()
                .map(|blocked| format!(
                    "{} (`{}` for provider `{}`)",
                    blocked.task_id, blocked.program, blocked.provider
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        steps.push(
            "Install the missing launcher where dispatch looks (`orbit doctor providers`), then \
             run `orbit task recheck-blocked --confirm`."
                .to_string(),
        );
    }
    actionable_check(
        "infra-blocked-tasks",
        WorkspaceDoctorStatus::Warning,
        clauses.join("; "),
        steps.join(" "),
    )
}

/// Render partitions whose checkout could not be resolved, adding the
/// filesystem failure that stopped the answer to the usual description.
pub(super) fn describe_unreachable_partitions(
    partitions: &[task_store::UnreachablePartition],
) -> String {
    partitions
        .iter()
        .map(|unreachable| {
            format!(
                "{} [{}]",
                describe_partitions(std::slice::from_ref(&unreachable.partition)),
                unreachable.reason
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render unclaimed partitions for a diagnostic line: each one's workspace id,
/// its path on disk, and how much task data deleting it would cost.
pub(super) fn describe_partitions(partitions: &[task_store::UnclaimedPartition]) -> String {
    partitions
        .iter()
        .map(|partition| {
            format!(
                "{} ({}, {} task bundle(s))",
                task_store::partition_id_of(&partition.path).unwrap_or("<unnamed>"),
                partition.path.display(),
                partition.task_bundles
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Directories named for a valid task id that never published `task.yaml`.
/// Split by the store's `is_unpublished_stub` predicate: empty / lock-only
/// residue is reapable; any other entry is retained unresolved task data.
pub(super) fn doctor_check_unpublished_bundle_dirs(
    runtime: &OrbitRuntime,
) -> [WorkspaceDoctorResult; 2] {
    match collect_unpublished_bundle_dirs(&runtime.global_root()) {
        Ok(scan) => [
            unpublished_stub_row(scan.stubs),
            unresolved_bundle_row(scan.unresolved),
        ],
        Err(error) => [
            check(
                "empty-task-stubs",
                WorkspaceDoctorStatus::Warning,
                format!("cannot scan task-store partitions for unpublished bundle stubs: {error}"),
            ),
            check(
                "unresolved-task-bundles",
                WorkspaceDoctorStatus::Warning,
                format!("cannot scan task-store partitions for unresolved task bundles: {error}"),
            ),
        ],
    }
}

pub(super) fn unpublished_stub_row(stubs: Vec<PathBuf>) -> WorkspaceDoctorResult {
    if stubs.is_empty() {
        return check(
            "empty-task-stubs",
            WorkspaceDoctorStatus::Ok,
            "no unpublished task-bundle stub directories".to_string(),
        );
    }
    let noun = if stubs.len() == 1 {
        "directory"
    } else {
        "directories"
    };
    actionable_check(
        "empty-task-stubs",
        WorkspaceDoctorStatus::Warning,
        format!(
            "{} unpublished task-bundle stub {noun} (empty or only .task.yaml.lock): {}",
            stubs.len(),
            join_paths(&stubs)
        ),
        "Run `orbit task reindex` from the owning checkout to skip or remove empty stub directories."
            .to_string(),
    )
}

pub(super) fn unresolved_bundle_row(unresolved: Vec<PathBuf>) -> WorkspaceDoctorResult {
    if unresolved.is_empty() {
        return check(
            "unresolved-task-bundles",
            WorkspaceDoctorStatus::Ok,
            "no unresolved task-bundle directories missing task.yaml".to_string(),
        );
    }
    let noun = if unresolved.len() == 1 {
        "directory"
    } else {
        "directories"
    };
    actionable_check(
        "unresolved-task-bundles",
        WorkspaceDoctorStatus::Warning,
        format!(
            "{} unresolved task-bundle {noun} (retained task data, no task.yaml): {}",
            unresolved.len(),
            join_paths(&unresolved)
        ),
        "Restore `task.yaml` from a backup or export, or move the directory aside deliberately. Do not delete it: it holds retained task data."
            .to_string(),
    )
}

pub(super) fn join_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

pub(super) struct UnpublishedBundleScan {
    stubs: Vec<PathBuf>,
    unresolved: Vec<PathBuf>,
}

pub(super) fn collect_unpublished_bundle_dirs(
    global_root: &Path,
) -> Result<UnpublishedBundleScan, OrbitError> {
    let workspaces_dir = task_store::task_workspaces_dir(global_root);
    let partitions = match std::fs::read_dir(&workspaces_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(UnpublishedBundleScan {
                stubs: Vec::new(),
                unresolved: Vec::new(),
            });
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "read {}: {error}",
                workspaces_dir.display()
            )));
        }
    };
    let mut stubs = Vec::new();
    let mut unresolved = Vec::new();
    for partition in partitions.flatten() {
        let partition = partition.path();
        if !partition.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&partition) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !path.is_dir() || !is_valid_orb_task_id(name) {
                continue;
            }
            // Shared with reindex/listing: residue-only dirs are stubs.
            if orbit_store::is_unpublished_stub(&path) {
                stubs.push(path);
            } else if !path.join(TASK_ENVELOPE_FILE_NAME).is_file() {
                unresolved.push(path);
            }
        }
    }
    stubs.sort();
    unresolved.sort();
    Ok(UnpublishedBundleScan { stubs, unresolved })
}

/// Delete one dead-holder lock only after acquiring its advisory lock. A
/// fresh holder can win the race between the initial scan and this cleanup;
/// in that case `try_lock_exclusive` reports contention and the file remains.
pub(super) fn remove_stale_lock_file(path: &Path) -> Result<bool, OrbitError> {
    let Some(holder) = orbit_store::read_lock_holder(path) else {
        return Ok(false);
    };
    if process_is_alive(holder.pid) {
        return Ok(false);
    }

    let file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "open stale lock candidate {}: {error}",
                path.display()
            )));
        }
    };
    match file.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
        Err(error) => {
            return Err(OrbitError::Store(format!(
                "acquire stale lock candidate {}: {error}",
                path.display()
            )));
        }
    }

    // Re-read after acquiring the advisory lock so a holder that appeared
    // after the first liveness probe is never removed.
    let should_remove = orbit_store::read_lock_holder(path)
        .is_some_and(|current_holder| !process_is_alive(current_holder.pid));
    if !should_remove {
        return Ok(false);
    }

    std::fs::remove_file(path).map_err(|error| {
        OrbitError::Io(format!(
            "remove stale lock candidate {}: {error}",
            path.display()
        ))
    })?;
    Ok(true)
}

/// Remove one fixed workspace-relative subtree without following a symlink at
/// the subtree boundary. The relative path is validated even though current
/// callers pass constants, keeping future cleanup additions inside the
/// resolved Orbit root by construction.
pub(super) fn remove_workspace_subtree(root: &Path, relative: &Path) -> Result<bool, OrbitError> {
    if relative.is_absolute()
        || relative.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(OrbitError::InvalidInput(format!(
            "cleanup path '{}' must remain relative to Orbit root '{}'",
            relative.display(),
            root.display()
        )));
    }
    let target = root.join(relative);
    let metadata = match std::fs::symlink_metadata(&target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "inspect retired graph state {}: {error}",
                target.display()
            )));
        }
    };
    let result = if metadata.file_type().is_symlink() || !metadata.is_dir() {
        std::fs::remove_file(&target)
    } else {
        std::fs::remove_dir_all(&target)
    };
    match result {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(OrbitError::Io(format!(
            "remove retired graph state {}: {error}",
            target.display()
        ))),
    }
}
