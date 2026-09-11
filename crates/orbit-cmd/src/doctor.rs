//! Workspace-level self-diagnostics behind `orbit doctor` [ORB-10005].
//!
//! Complements the narrower `orbit skill doctor` / `orbit tool doctor`
//! surfaces with whole-workspace checks: config validity, store database
//! integrity and schema-ledger version, free disk space on the volume
//! holding `.orbit`, semantic-index staleness, leftover lock files from
//! crashed holders, orphaned `running`/`pending` job runs, task
//! reservations whose owner or terminal task association is conclusively
//! inactive, task
//! relation/dependency targets that no longer resolve in the registry
//! (grandfathered relations that block index rebuilds — ORB-10305).
//!
//! Every check degrades rather than errors: subsystems that are absent in a
//! fresh workspace report [`WorkspaceDoctorStatus::Skipped`], and probe
//! failures become `Warning`/`Error` rows instead of aborting the whole
//! diagnosis. The cheap probes shared with the dashboard's
//! `/healthz?detailed=true` ([`DoctorCommands::health_check_store_writable`])
//! also live here.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_core::application::artifact_health::{ArtifactFinding, RetiredActivityBackendRepair};
use orbit_store::maintenance::migration::SUPPORTED_SCHEMA_VERSION;
use orbit_types::workspace::WorkspacePaths;
use serde::Serialize;

use crate::task_store;

/// Outcome of one workspace doctor check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceDoctorStatus {
    /// Check passed.
    Ok,
    /// Something is off but the workspace remains usable.
    Warning,
    /// The workspace is unhealthy; `orbit doctor` exits nonzero.
    Error,
    /// The subsystem is absent (fresh workspace) — nothing to check.
    Skipped,
}

/// One row of `orbit doctor` output.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceDoctorResult {
    /// Stable check identifier (e.g. `config`, `database`, `disk-space`).
    pub check_name: String,
    /// Pass/warn/fail/skip outcome.
    pub status: WorkspaceDoctorStatus,
    /// Human-readable detail line.
    pub message: String,
    /// Exact repair command or explicit manual next step for warning/error rows.
    pub remediation: Option<String>,
}

fn check(name: &str, status: WorkspaceDoctorStatus, message: String) -> WorkspaceDoctorResult {
    let remediation = matches!(
        status,
        WorkspaceDoctorStatus::Warning | WorkspaceDoctorStatus::Error
    )
    .then(|| {
        "Address the condition named in the diagnostic details, then rerun `orbit doctor`."
            .to_string()
    });
    WorkspaceDoctorResult {
        check_name: name.to_string(),
        status,
        message,
        remediation,
    }
}

fn actionable_check(
    name: &str,
    status: WorkspaceDoctorStatus,
    message: String,
    remediation: String,
) -> WorkspaceDoctorResult {
    WorkspaceDoctorResult {
        check_name: name.to_string(),
        status,
        message,
        remediation: Some(remediation),
    }
}

/// Warn when the volume holding `.orbit` has less than this many free bytes.
const DISK_WARN_BYTES: u64 = 1024 * 1024 * 1024; // 1 GiB
/// Fail when the volume holding `.orbit` has less than this many free bytes.
const DISK_FAIL_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB
/// Warn when less than this percentage of the volume is free.
const DISK_WARN_PCT: f64 = 5.0;
/// Fail when less than this percentage of the volume is free.
const DISK_FAIL_PCT: f64 = 1.0;

/// Workspace doctor / health-probe command surface for [`OrbitRuntime`]
/// (extension trait — the implementation moved out of orbit-core in
/// [ORB-10016]).
pub trait DoctorCommands {
    /// Run every workspace-level doctor check. Individual checks never abort
    /// the diagnosis: probe failures surface as `Warning`/`Error` rows and
    /// absent subsystems as `Skipped`.
    fn doctor_workspace(&self) -> Result<Vec<WorkspaceDoctorResult>, OrbitError>;

    /// Remove lock files left by dead holders, without disturbing a lock that
    /// is currently held by another process.
    fn remove_stale_lock_files(&self) -> Result<usize, OrbitError>;

    /// Release reservations that remain conclusively stale after a write-boundary recheck.
    fn clear_stale_task_reservations(&self) -> Result<usize, OrbitError>;

    /// Remove retired graph state from the exact worktree-local and shared
    /// workspace locations. Missing locations are a successful no-op.
    fn remove_retired_graph_state(&self) -> Result<usize, OrbitError>;

    /// Retire deprecated definition artifacts whose recorded digest proves
    /// Orbit wrote them, preserving locally modified ones outside the active
    /// catalog. Faulty and user-authored artifacts are never touched.
    fn remove_stale_definition_artifacts(&self) -> Result<usize, OrbitError>;

    /// Remove known retired `spec.backend` values from schemaVersion 2
    /// agent-loop activities. Unknown backends and unrelated malformed
    /// files are left untouched.
    fn repair_retired_activity_backends(&self) -> Result<RetiredActivityBackendRepair, OrbitError>;

    /// Delete task-store partitions under `<global_root>/tasks/workspaces/`
    /// whose workspace id is no longer present in the registry — left behind
    /// by a `workspace teardown` run on an older binary, or by removing a
    /// checkout without running teardown [ORB-12109].
    fn remove_orphan_task_stores(&self) -> Result<usize, OrbitError>;

    /// Cheap store write probe for health endpoints: open the store and
    /// acquire + roll back the write lock without mutating anything.
    fn health_check_store_writable(&self) -> Result<String, OrbitError>;
}

impl DoctorCommands for OrbitRuntime {
    fn doctor_workspace(&self) -> Result<Vec<WorkspaceDoctorResult>, OrbitError> {
        let mut results = vec![
            doctor_check_config(self),
            doctor_check_database(self),
            doctor_check_disk_space(self),
            doctor_check_semantic_index(self),
            doctor_check_stale_locks(self),
            doctor_check_job_runs(self),
            doctor_check_task_reservations(self),
            doctor_check_task_relations(self),
            doctor_check_orphan_task_stores(self),
        ];
        results.extend(doctor_check_definition_artifacts(self));
        Ok(results)
    }

    fn remove_stale_lock_files(&self) -> Result<usize, OrbitError> {
        let mut removed = 0;
        for path in collect_lock_files(self.paths()) {
            if remove_stale_lock_file(&path)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn clear_stale_task_reservations(&self) -> Result<usize, OrbitError> {
        self.release_stale_task_reservations()
    }

    fn remove_stale_definition_artifacts(&self) -> Result<usize, OrbitError> {
        OrbitRuntime::remove_stale_definition_artifacts(self)
    }

    fn repair_retired_activity_backends(&self) -> Result<RetiredActivityBackendRepair, OrbitError> {
        OrbitRuntime::repair_retired_activity_backends(self)
    }

    fn remove_orphan_task_stores(&self) -> Result<usize, OrbitError> {
        let removed = crate::task_store::remove_unclaimed_task_stores(&self.global_root())?;
        Ok(removed.len())
    }

    fn remove_retired_graph_state(&self) -> Result<usize, OrbitError> {
        let targets = [
            (self.local_root(), Path::new("graph")),
            (self.shared_root(), Path::new("knowledge/graph")),
        ];
        let mut removed = 0;
        for (root, relative) in targets {
            if remove_workspace_subtree(&root, relative)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn health_check_store_writable(&self) -> Result<String, OrbitError> {
        self.check_sqlite_store_writable()?;
        Ok("store database accepts writes".to_string())
    }
}

/// Parse + validate the effective (workspace-over-global) `config.toml`.
fn doctor_check_config(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let path = match runtime.config_path() {
        Ok(path) => path,
        Err(error) => {
            return check(
                "config",
                WorkspaceDoctorStatus::Error,
                format!("cannot select effective config: {error}"),
            );
        }
    };
    match orbit_config::validate_layered_config(&orbit_config::ConfigRoots::new(
        runtime.global_root(),
        runtime.data_root(),
    )) {
        Ok(_) => check(
            "config",
            WorkspaceDoctorStatus::Ok,
            format!("valid ({})", path.display()),
        ),
        Err(error) => check(
            "config",
            WorkspaceDoctorStatus::Error,
            format!("invalid ({}): {error}", path.display()),
        ),
    }
}

/// `PRAGMA quick_check` plus migration-ledger schema version vs binary.
fn doctor_check_database(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
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
fn doctor_check_disk_space(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let root = runtime.local_root();
    disk_space_check(&root)
}

/// Semantic (docs/tasks) embedding index staleness, using the
/// stale-row signal the vector store already tracks.
fn doctor_check_semantic_index(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    match runtime.semantic_stats() {
        Err(error) => check(
            "semantic-index",
            WorkspaceDoctorStatus::Warning,
            format!("cannot read semantic index: {error}"),
        ),
        Ok(stats) => {
            let total: usize = stats.rows.counts.iter().map(|count| count.rows).sum();
            if total == 0 {
                check(
                    "semantic-index",
                    WorkspaceDoctorStatus::Skipped,
                    "no semantic embeddings indexed yet".to_string(),
                )
            } else if stats.rows.stale_rows > 0 {
                actionable_check(
                    "semantic-index",
                    WorkspaceDoctorStatus::Warning,
                    format!(
                        "{} of {total} embedding rows are stale; re-run `orbit semantic index`",
                        stats.rows.stale_rows
                    ),
                    "Run `orbit semantic index`, then rerun `orbit doctor`.".to_string(),
                )
            } else {
                check(
                    "semantic-index",
                    WorkspaceDoctorStatus::Ok,
                    format!("{total} embedding rows, none stale"),
                )
            }
        }
    }
}

/// Lock files whose recorded holder PID is dead. Clean lock-guard releases
/// clear their diagnostic metadata while still holding the advisory `flock`,
/// so metadata found here is from an interrupted/crashed holder — a crash
/// signal, not an availability problem.
fn doctor_check_stale_locks(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
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
fn doctor_check_task_reservations(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
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

/// Task-store partitions under `<global_root>/tasks/workspaces/<ws_id>/`
/// that no registry claims — left behind by a `workspace teardown` run on an
/// older binary, or by deleting a checkout without running teardown
/// [ORB-12109]. Scoped to the whole host, not just this workspace, because the
/// partition directory is itself host-global.
///
/// A partition is named for its *task-registry* workspace id, so the task
/// registry is what claims it; the workspace catalog and the synthetic
/// `--root` partition are the other claimants. Comparing the directory name
/// against catalog `ws_*` ids alone reported every `<slug>-<hash>` partition —
/// including live ones — as orphaned [ORB-12119].
fn doctor_check_orphan_task_stores(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
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

    if partitions.unclaimed.is_empty() {
        return check(
            "orphan-task-stores",
            WorkspaceDoctorStatus::Ok,
            format!(
                "{} task-store partition(s) scanned, all claimed by a workspace binding",
                partitions.scanned
            ),
        );
    }

    let orphans = partitions
        .unclaimed
        .iter()
        .map(|path| {
            format!(
                "{} ({}, {} task bundle(s))",
                task_store::partition_id(path).unwrap_or("<unnamed>"),
                path.display(),
                count_task_bundles(path)
            )
        })
        .collect::<Vec<_>>();

    actionable_check(
        "orphan-task-stores",
        WorkspaceDoctorStatus::Warning,
        format!(
            "{} orphaned task-store partition(s) (no workspace binding claims them): {}",
            orphans.len(),
            orphans.join("; ")
        ),
        "Run `orbit doctor --fix-orphan-task-stores --confirm`.".to_string(),
    )
}

/// Task bundles directly under one workspace's task-store partition — each
/// is a subdirectory named for its task id.
fn count_task_bundles(partition: &Path) -> usize {
    std::fs::read_dir(partition)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .count()
}

/// Delete one dead-holder lock only after acquiring its advisory lock. A
/// fresh holder can win the race between the initial scan and this cleanup;
/// in that case `try_lock_exclusive` reports contention and the file remains.
fn remove_stale_lock_file(path: &Path) -> Result<bool, OrbitError> {
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
fn remove_workspace_subtree(root: &Path, relative: &Path) -> Result<bool, OrbitError> {
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

/// `running`/`pending` job runs with no live worker process — dead recorded
/// owner, or a queued run no worker ever claimed (read-only view of the
/// reconcile signal; see `job/run/reconcile.rs`) [ORB-10070].
fn doctor_check_job_runs(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let running = match runtime.list_orphaned_running_job_runs() {
        Ok(orphans) => orphans,
        Err(error) => {
            return check(
                "job-runs",
                WorkspaceDoctorStatus::Warning,
                format!("cannot inspect job runs: {error}"),
            );
        }
    };
    let pending = match runtime.list_orphaned_pending_job_runs() {
        Ok(orphans) => orphans,
        Err(error) => {
            return check(
                "job-runs",
                WorkspaceDoctorStatus::Warning,
                format!("cannot inspect job runs: {error}"),
            );
        }
    };
    if running.is_empty() && pending.is_empty() {
        return check(
            "job-runs",
            WorkspaceDoctorStatus::Ok,
            "no orphaned running or pending job runs".to_string(),
        );
    }
    let mut segments = Vec::new();
    if !running.is_empty() {
        let ids: Vec<&str> = running.iter().map(|run| run.run_id.as_str()).collect();
        segments.push(format!(
            "{} running run(s) whose owner process is gone: {} — they \
             finalize as `interrupted` on reconcile; resume with \
             `orbit job resume <run_id>`",
            running.len(),
            ids.join(", ")
        ));
    }
    if !pending.is_empty() {
        let ids: Vec<&str> = pending.iter().map(|run| run.run_id.as_str()).collect();
        segments.push(format!(
            "{} pending run(s) with no live worker process: {} — they \
             finalize as `interrupted` on reconcile; clear immediately with \
             `orbit run cancel <run_id>`",
            pending.len(),
            ids.join(", ")
        ));
    }
    actionable_check(
        "job-runs",
        WorkspaceDoctorStatus::Warning,
        segments.join("; "),
        "For each named running run use `orbit job resume <run_id>`; for each named pending run use `orbit run cancel <run_id>`.".to_string(),
    )
}

/// Task relation/dependency targets that no longer resolve to a registered
/// task bundle — the "grandfathered" relations that make a generated task
/// index fail to rebuild against its relation validator, forcing an unbounded
/// bundle-scan fallback (ORB-10305). Scoped to the current
/// workspace; surfacing them here lets an operator fix or remove the offending
/// relation before the validator trips over it at rebuild time.
fn doctor_check_task_relations(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    let workspace_id = match runtime.workspace_id() {
        Ok(id) => id,
        Err(error) => {
            return check(
                "task-relations",
                WorkspaceDoctorStatus::Warning,
                format!("cannot resolve workspace id to audit relations: {error}"),
            );
        }
    };
    match runtime.audit_dangling_relations(Some(&workspace_id)) {
        Err(error) => check(
            "task-relations",
            WorkspaceDoctorStatus::Warning,
            format!("cannot audit task relations: {error}"),
        ),
        Ok(dangling) if dangling.is_empty() => check(
            "task-relations",
            WorkspaceDoctorStatus::Ok,
            "no unresolved relation/dependency targets".to_string(),
        ),
        Ok(dangling) => {
            let detail = dangling
                .iter()
                .map(|target| {
                    format!(
                        "{} ({}) -> {}",
                        target.source_task_id, target.relation_type, target.target_task_id
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            actionable_check(
                "task-relations",
                WorkspaceDoctorStatus::Warning,
                format!(
                    "{} unresolved relation/dependency target(s) will block index rebuild \
                     until fixed or removed: {detail}",
                    dangling.len()
                ),
                "Inspect each named source with `orbit task show <task-id>` and update or remove its unresolved relation/dependency target.".to_string(),
            )
        }
    }
}

/// Definition-artifact health — one row per artifact kind (skills, jobs,
/// activities, auto-tasks, routines) [ORB-10800].
///
/// Severity is deliberately asymmetric. A workspace-authored definition that
/// fails to parse is the operator's own in-progress edit, and classifying it
/// as `Error` would silently start failing `orbit doctor` in cron and CI for
/// workspaces that were passing yesterday. Only an *unloadable shipped
/// default* — a broken install: Orbit-written content that no longer parses,
/// or a primary shipped default that is simply gone — escalates to `Error`.
/// Everything else warns.
fn doctor_check_definition_artifacts(runtime: &OrbitRuntime) -> Vec<WorkspaceDoctorResult> {
    let report = match runtime.inspect_definition_artifacts() {
        Ok(report) => report,
        Err(error) => {
            return vec![actionable_check(
                "artifacts",
                WorkspaceDoctorStatus::Warning,
                format!("cannot inspect definition artifacts: {error}"),
                "Resolve the store/runtime error, then rerun `orbit doctor`.".to_string(),
            )];
        }
    };

    report
        .into_iter()
        .map(|health| {
            let check_name = format!("artifacts-{}", health.kind.as_str());
            if health.findings.is_empty() {
                let message = if health.scanned == 0 {
                    format!("no {} on disk yet", health.kind.as_str())
                } else {
                    format!(
                        "{} {} loaded, none residual, stale, deprecated, faulty, or missing",
                        health.scanned,
                        health.kind.as_str()
                    )
                };
                let status = if health.scanned == 0 {
                    WorkspaceDoctorStatus::Skipped
                } else {
                    WorkspaceDoctorStatus::Ok
                };
                return check(&check_name, status, message);
            }

            let status = if health
                .findings
                .iter()
                .any(ArtifactFinding::is_unloadable_shipped_default)
            {
                WorkspaceDoctorStatus::Error
            } else {
                WorkspaceDoctorStatus::Warning
            };
            let detail = health
                .findings
                .iter()
                .map(|finding| format!("{}: {}", finding.condition.as_str(), finding.detail))
                .collect::<Vec<_>>()
                .join("; ");
            // Every finding carries its own exact repair command; dedupe so a
            // kind with five stale copies names one command, not five.
            let mut remediations: Vec<&str> = Vec::new();
            for finding in &health.findings {
                let remediation = finding.remediation.as_str();
                if !remediations.contains(&remediation) {
                    remediations.push(remediation);
                }
            }
            actionable_check(
                &check_name,
                status,
                format!(
                    "{} of {} {} need attention — {detail}",
                    health.findings.len(),
                    health.scanned,
                    health.kind.as_str()
                ),
                remediations.join(" "),
            )
        })
        .collect()
}

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

fn human_bytes(bytes: u64) -> String {
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
