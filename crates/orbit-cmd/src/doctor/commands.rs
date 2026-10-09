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
    /// A fact about the setup, neither a pass nor a problem: what Orbit can
    /// and cannot observe here.
    Info,
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
    /// Wall-clock duration of the probe in milliseconds. Rows from one probe share its duration.
    pub duration_ms: u64,
}

impl WorkspaceDoctorResult {
    /// Measure a single diagnostic, including unsuccessful or skipped outcomes.
    pub fn timed(probe: impl FnOnce() -> Self) -> Self {
        let start = std::time::Instant::now();
        let mut row = probe();
        row.duration_ms = elapsed_ms(start);
        row
    }

    /// Measure a probe that expands into several rows (for example config findings).
    /// Each row reports the shared probe duration, so these values are not additive.
    pub fn timed_many<I: IntoIterator<Item = Self>>(probe: impl FnOnce() -> I) -> Vec<Self> {
        let start = std::time::Instant::now();
        let mut rows = probe().into_iter().collect::<Vec<_>>();
        let duration_ms = elapsed_ms(start);
        for row in &mut rows {
            row.duration_ms = duration_ms;
        }
        rows
    }
}

fn elapsed_ms(start: std::time::Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
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
        duration_ms: 0,
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
        duration_ms: 0,
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

/// One read-only doctor check. The check is a plain function, so a caller can
/// run each probe on its own thread and bound its time, as the dashboard does.
#[derive(Clone, Copy)]
pub struct DoctorProbe {
    /// The check name its row carries, or the shared prefix of a probe that
    /// expands into several rows. Names the probe when it cannot report.
    pub name: &'static str,
    check: ProbeCheck,
}

#[derive(Clone, Copy)]
enum ProbeCheck {
    One(fn(&OrbitRuntime, bool) -> WorkspaceDoctorResult),
    Many(fn(&OrbitRuntime, bool) -> Vec<WorkspaceDoctorResult>),
}

impl DoctorProbe {
    pub(crate) const fn one(
        name: &'static str,
        check: fn(&OrbitRuntime, bool) -> WorkspaceDoctorResult,
    ) -> Self {
        Self {
            name,
            check: ProbeCheck::One(check),
        }
    }

    pub(crate) const fn many(
        name: &'static str,
        check: fn(&OrbitRuntime, bool) -> Vec<WorkspaceDoctorResult>,
    ) -> Self {
        Self {
            name,
            check: ProbeCheck::Many(check),
        }
    }

    /// Run the check and time it. `deep` scans every database page; only the
    /// database probe reads it.
    pub fn run(&self, runtime: &OrbitRuntime, deep: bool) -> Vec<WorkspaceDoctorResult> {
        match self.check {
            ProbeCheck::One(check) => vec![WorkspaceDoctorResult::timed(|| check(runtime, deep))],
            ProbeCheck::Many(check) => WorkspaceDoctorResult::timed_many(|| check(runtime, deep)),
        }
    }
}

/// The workspace checks, in report order. Individual checks never abort the
/// diagnosis: probe failures surface as `Warning`/`Error` rows and absent
/// subsystems as `Skipped`.
pub(crate) const WORKSPACE_PROBES: &[DoctorProbe] = &[
    DoctorProbe::many("config", |runtime, _| doctor_check_config(runtime)),
    DoctorProbe::one("database", doctor_check_database),
    DoctorProbe::one("disk-space", |runtime, _| doctor_check_disk_space(runtime)),
    DoctorProbe::one("search-index", |runtime, _| {
        doctor_check_search_index(runtime)
    }),
    DoctorProbe::one("stale-locks", |runtime, _| {
        doctor_check_stale_locks(runtime)
    }),
    DoctorProbe::one("job-runs", |runtime, _| doctor_check_job_runs(runtime)),
    DoctorProbe::one("pull-settlements", |runtime, _| {
        doctor_check_pull_settlements(runtime)
    }),
    DoctorProbe::one("pull-protocol", |runtime, _| {
        doctor_check_pull_protocol(runtime)
    }),
    DoctorProbe::one("task-reservations", |runtime, _| {
        doctor_check_task_reservations(runtime)
    }),
    DoctorProbe::one("task-relations", |runtime, _| {
        doctor_check_task_relations(runtime)
    }),
    DoctorProbe::one("infra-blocked-tasks", |runtime, _| {
        doctor_check_infra_blocked_tasks(runtime)
    }),
    DoctorProbe::one("blocked-task-recovery", |runtime, _| {
        doctor_check_blocked_task_recovery(runtime)
    }),
    DoctorProbe::one("automation-consumers", |runtime, _| {
        doctor_check_stalled_automation(runtime)
    }),
    DoctorProbe::one("review", |runtime, _| doctor_check_review(runtime)),
    DoctorProbe::one("forge-remote", |runtime, _| {
        doctor_check_forge_remote(runtime)
    }),
    DoctorProbe::one("host-shutdown", |runtime, _| {
        doctor_check_host_shutdown(runtime)
    }),
    DoctorProbe::one("env-pass", |runtime, _| doctor_check_env_pass(runtime)),
    DoctorProbe::one("validation-env", |runtime, _| {
        doctor_check_validation_env(runtime)
    }),
    DoctorProbe::one("orphan-task-stores", |runtime, _| {
        doctor_check_orphan_task_stores(runtime)
    }),
    DoctorProbe::one("tracked-orbit-files", |runtime, _| {
        doctor_check_tracked_orbit_files(runtime)
    }),
    DoctorProbe::one("plugin-builds", |runtime, _| {
        doctor_check_plugin_builds(runtime)
    }),
    DoctorProbe::one("store-retention", |runtime, _| {
        doctor_check_store_retention(runtime)
    }),
    DoctorProbe::one("worktree-reclaim", |runtime, _| {
        doctor_check_worktree_reclaim(runtime)
    }),
    DoctorProbe::many("task-bundles", |runtime, _| {
        doctor_check_unpublished_bundle_dirs(runtime).into()
    }),
    DoctorProbe::many("artifacts", |runtime, _| {
        doctor_check_definition_artifacts(runtime)
    }),
];

/// Workspace doctor / health-probe command surface for [`OrbitRuntime`]
/// (extension trait — the implementation moved out of orbit-core in
/// [ORB-10016]).
pub trait DoctorCommands {
    /// Restrict writable Orbit-owned state directories to owner-only access,
    /// excluding run worktrees, Cargo target trees and child symlinks.
    fn repair_state_directory_permissions(&self) -> Result<usize, OrbitError>;
    /// Run every workspace-level doctor check. Individual checks never abort
    /// the diagnosis: probe failures surface as `Warning`/`Error` rows and
    /// absent subsystems as `Skipped`.
    fn doctor_workspace(&self) -> Result<Vec<WorkspaceDoctorResult>, OrbitError> {
        self.doctor_workspace_with_depth(false)
    }

    /// Run workspace diagnostics, optionally scanning every database page with SQLite quick_check.
    fn doctor_workspace_with_depth(
        &self,
        deep: bool,
    ) -> Result<Vec<WorkspaceDoctorResult>, OrbitError>;

    /// Clear records left by dead holders, without disturbing a lock that
    /// is currently held by another process. Lock files remain in place so
    /// queued openers keep sharing the same inode.
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
    fn repair_state_directory_permissions(&self) -> Result<usize, OrbitError> {
        super::permissions::repair_state_directory_permissions(self)
    }
    fn doctor_workspace_with_depth(
        &self,
        deep: bool,
    ) -> Result<Vec<WorkspaceDoctorResult>, OrbitError> {
        Ok(WORKSPACE_PROBES
            .iter()
            .flat_map(|probe| probe.run(self, deep))
            .collect())
    }

    fn remove_stale_lock_files(&self) -> Result<usize, OrbitError> {
        let mut cleared = 0;
        for path in collect_lock_files(self.paths()) {
            if remove_stale_lock_file(&path)? {
                cleared += 1;
            }
        }
        Ok(cleared)
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
