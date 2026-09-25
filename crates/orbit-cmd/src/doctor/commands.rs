use super::*;

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

pub(super) fn check(
    name: &str,
    status: WorkspaceDoctorStatus,
    message: String,
) -> WorkspaceDoctorResult {
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

pub(super) fn actionable_check(
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

/// Outcome of `--fix-orphan-task-stores`, split by whether a removed
/// partition held task bundles, so the operator-facing report never calls a
/// populated partition empty [ORB-12144].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrphanTaskStoreRemoval {
    /// Empty partitions removed — deleting these cost no task data.
    pub empty_partitions: usize,
    /// Populated partitions removed because their bound checkout was
    /// confirmed gone.
    pub populated_partitions: usize,
    /// Task bundles destroyed by removing `populated_partitions`.
    pub task_bundles: usize,
}

/// Warn when the volume holding `.orbit` has less than this many free bytes.
pub(super) const DISK_WARN_BYTES: u64 = 1024 * 1024 * 1024; // 1 GiB
/// Fail when the volume holding `.orbit` has less than this many free bytes.
pub(super) const DISK_FAIL_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB
/// Warn when less than this percentage of the volume is free.
pub(super) const DISK_WARN_PCT: f64 = 5.0;
/// Fail when less than this percentage of the volume is free.
pub(super) const DISK_FAIL_PCT: f64 = 1.0;

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
    /// checkout without running teardown [ORB-12109]. Partitions that are
    /// unowned and still hold task bundles are reported but never deleted
    /// [ORB-12131]; partitions whose bound checkout is confirmed gone are
    /// deleted along with their task bundles [ORB-12143].
    fn remove_orphan_task_stores(&self) -> Result<OrphanTaskStoreRemoval, OrbitError>;

    /// Cheap store write probe for health endpoints: open the store and
    /// acquire + roll back the write lock without mutating anything.
    fn health_check_store_writable(&self) -> Result<String, OrbitError>;
}

impl DoctorCommands for OrbitRuntime {
    fn doctor_workspace(&self) -> Result<Vec<WorkspaceDoctorResult>, OrbitError> {
        let mut results = doctor_check_config(self);
        results.extend([
            doctor_check_database(self),
            doctor_check_disk_space(self),
            doctor_check_search_index(self),
            doctor_check_stale_locks(self),
            doctor_check_job_runs(self),
            doctor_check_task_reservations(self),
            doctor_check_task_relations(self),
            doctor_check_stalled_automation(self),
            doctor_check_host_shutdown(self),
            doctor_check_orphan_task_stores(self),
            doctor_check_tracked_orbit_files(self),
        ]);
        results.extend(doctor_check_unpublished_bundle_dirs(self));
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

    fn remove_orphan_task_stores(&self) -> Result<OrphanTaskStoreRemoval, OrbitError> {
        let removed = crate::task_store::remove_unclaimed_task_stores(&self.global_root())?;
        Ok(OrphanTaskStoreRemoval {
            empty_partitions: removed.empty.len(),
            populated_partitions: removed.stale.len(),
            task_bundles: removed.task_bundles_removed(),
        })
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
