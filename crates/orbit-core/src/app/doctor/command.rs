use super::*;

/// Warn when the volume holding `.orbit` has less than this many free bytes.
pub(super) const DISK_WARN_BYTES: u64 = 1024 * 1024 * 1024; // 1 GiB

/// Fail when the volume holding `.orbit` has less than this many free bytes.
pub(super) const DISK_FAIL_BYTES: u64 = 256 * 1024 * 1024; // 256 MiB

/// Warn when less than this percentage of the volume is free.
pub(super) const DISK_WARN_PCT: f64 = 5.0;

/// Fail when less than this percentage of the volume is free.
pub(super) const DISK_FAIL_PCT: f64 = 1.0;

/// The workspace checks, in report order. Individual checks never abort the
/// diagnosis: probe failures surface as `Warning`/`Error` rows and absent
/// subsystems as `Skipped`.
pub(crate) const WORKSPACE_PROBES: &[DoctorProbe] = &[
    DoctorProbe::many("config", |probe| doctor_check_config(probe.runtime)),
    DoctorProbe::many("retired-config-keys", |probe| {
        super::retired_config::doctor_check_retired_config_keys(probe.runtime)
    }),
    DoctorProbe::many("retired-scripts", |probe| {
        super::retired_config::doctor_check_retired_scripts(probe.runtime)
    }),
    DoctorProbe::one("database", |probe| {
        doctor_check_database(probe.runtime, probe.deep)
    }),
    DoctorProbe::one("disk-space", |probe| doctor_check_disk_space(probe.runtime)),
    DoctorProbe::one("search-index", |probe| {
        doctor_check_search_index(probe.runtime)
    })
    .slow(),
    DoctorProbe::one("stale-locks", |probe| {
        doctor_check_stale_locks(probe.runtime)
    }),
    DoctorProbe::one("job-runs", |probe| doctor_check_job_runs(probe.runtime)),
    DoctorProbe::one("pull-settlements", |probe| {
        doctor_check_pull_settlements(probe.runtime)
    }),
    DoctorProbe::one("pull-protocol", |probe| {
        doctor_check_pull_protocol(probe.runtime)
    }),
    DoctorProbe::one("pull-gates", |probe| doctor_check_pull_gates(probe.runtime)),
    DoctorProbe::one("task-reservations", |probe| {
        doctor_check_task_reservations(probe.runtime)
    }),
    DoctorProbe::one("task-relations", |probe| {
        doctor_check_task_relations(probe.runtime)
    })
    .slow(),
    DoctorProbe::one("infra-blocked-tasks", |probe| {
        doctor_check_infra_blocked_tasks(probe.runtime)
    }),
    DoctorProbe::one("blocked-task-recovery", |probe| {
        doctor_check_blocked_task_recovery(probe.runtime)
    }),
    DoctorProbe::one("automation-consumers", |probe| {
        doctor_check_stalled_automation(probe.runtime)
    }),
    DoctorProbe::one("routine-freshness", |probe| {
        doctor_check_routine_freshness(probe.runtime, probe.host)
    })
    .slow(),
    DoctorProbe::one("review", |probe| doctor_check_review(probe.runtime)),
    DoctorProbe::one("forge-remote", |probe| {
        doctor_check_forge_remote(probe.runtime)
    }),
    DoctorProbe::one("host-shutdown", |probe| {
        doctor_check_host_shutdown(probe.runtime)
    }),
    DoctorProbe::one("env-pass", |probe| {
        doctor_check_env_pass(probe.runtime, probe.env, probe.host)
    }),
    DoctorProbe::one("claude-worker-token", |probe| {
        super::worker_token::doctor_check_claude_worker_token(probe.runtime)
    }),
    DoctorProbe::one("validation-env", |probe| {
        doctor_check_validation_env(probe.runtime)
    }),
    DoctorProbe::one("mid-run-steering", |probe| {
        doctor_check_mid_run_steering(probe.runtime)
    }),
    DoctorProbe::one("orphan-task-stores", |probe| {
        doctor_check_orphan_task_stores(probe.runtime, probe.host)
    }),
    DoctorProbe::one("tracked-orbit-files", |probe| {
        doctor_check_tracked_orbit_files(probe.runtime)
    }),
    DoctorProbe::one("plugin-builds", |probe| {
        doctor_check_plugin_builds(probe.runtime)
    }),
    DoctorProbe::one("store-retention", |probe| {
        doctor_check_store_retention(probe.runtime)
    })
    .slow(),
    DoctorProbe::one("worktree-reclaim", |probe| {
        doctor_check_worktree_reclaim(probe.runtime)
    })
    .slow(),
    DoctorProbe::many("task-bundles", |probe| {
        doctor_check_unpublished_bundle_dirs(probe.runtime).into()
    })
    .slow(),
    DoctorProbe::many("artifacts", |probe| {
        doctor_check_definition_artifacts(probe.runtime)
    }),
    DoctorProbe::many("seeding-policy", |probe| {
        super::seeding::doctor_check_seeding_policy(probe.runtime)
    }),
];

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

/// One read-only doctor check. The check is a plain function, so a caller can
/// run each probe on its own thread and bound its time, as the dashboard does.
#[derive(Clone, Copy)]
pub struct DoctorProbe {
    /// The check name its row carries, or the shared prefix of a probe that
    /// expands into several rows. Names the probe when it cannot report.
    pub name: &'static str,
    check: ProbeCheck,
    /// Walks a large tree or reads every task, so a parallel runner should
    /// start it first: the report cannot finish before its slowest probe.
    slow: bool,
}

/// What one probe reads: the workspace runtime, the caller's environment,
/// whether to scan every database page, and the host reads above this crate.
pub(crate) struct Probe<'a> {
    pub(crate) runtime: &'a OrbitRuntime,
    pub(crate) env: &'a orbit_common::Env,
    pub(crate) deep: bool,
    pub(crate) host: &'a dyn Host,
}

#[derive(Clone, Copy)]
enum ProbeCheck {
    One(fn(&Probe<'_>) -> WorkspaceDoctorResult),
    Many(fn(&Probe<'_>) -> Vec<WorkspaceDoctorResult>),
}

impl DoctorProbe {
    pub(crate) const fn one(
        name: &'static str,
        check: fn(&Probe<'_>) -> WorkspaceDoctorResult,
    ) -> Self {
        Self {
            name,
            check: ProbeCheck::One(check),
            slow: false,
        }
    }

    pub(crate) const fn many(
        name: &'static str,
        check: fn(&Probe<'_>) -> Vec<WorkspaceDoctorResult>,
    ) -> Self {
        Self {
            name,
            check: ProbeCheck::Many(check),
            slow: false,
        }
    }

    /// Mark a probe whose cost grows with the workspace (a tree walk or a scan
    /// of every task), so `run_doctor_report` starts it before the cheap ones.
    pub(crate) const fn slow(self) -> Self {
        Self { slow: true, ..self }
    }

    /// Whether `run_doctor_report` starts this probe before the cheap ones.
    pub(crate) const fn is_slow(&self) -> bool {
        self.slow
    }

    /// Run the check and time it. `deep` scans every database page; only the
    /// database probe reads it.
    pub fn run(
        &self,
        runtime: &OrbitRuntime,
        env: &orbit_common::Env,
        deep: bool,
        host: &dyn Host,
    ) -> Vec<WorkspaceDoctorResult> {
        let probe = Probe {
            runtime,
            env,
            deep,
            host,
        };
        match self.check {
            ProbeCheck::One(check) => vec![WorkspaceDoctorResult::timed(|| check(&probe))],
            ProbeCheck::Many(check) => WorkspaceDoctorResult::timed_many(|| check(&probe)),
        }
    }
}

/// The doctor's repairs and checks on `OrbitRuntime`, which the doctor
/// app's flows and the surfaces not yet routed through it call.
pub trait DoctorCommands {
    /// Restrict writable Orbit-owned state directories to owner-only access,
    /// excluding run worktrees, Cargo target trees and child symlinks.
    fn repair_state_directory_permissions(&self) -> Result<usize, OrbitError>;
    /// Run every workspace-level doctor check. Individual checks never abort
    /// the diagnosis: probe failures surface as `Warning`/`Error` rows and
    /// absent subsystems as `Skipped`.
    /// `deep` scans every database page with SQLite quick_check.
    fn doctor_workspace_with_depth(
        &self,
        env: &orbit_common::Env,
        deep: bool,
        host: &dyn Host,
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

    /// Delete task-store partitions under `<global_root>/tasks/workspaces/`
    /// whose workspace id is no longer present in the registry — left behind
    /// by a `workspace teardown` run on an older binary, or by removing a
    /// checkout without running teardown [ORB-12109]. Partitions that are
    /// unowned and still hold task bundles are reported but never deleted
    /// [ORB-12131]; partitions whose bound checkout is confirmed gone are
    /// deleted along with their task bundles [ORB-12143].
    fn remove_orphan_task_stores(
        &self,
        host: &dyn Host,
    ) -> Result<OrphanTaskStoreRemoval, OrbitError>;

    /// Cheap store write probe for health endpoints: open the store and
    /// acquire + roll back the write lock without mutating anything.
    fn health_check_store_writable(&self) -> Result<String, OrbitError>;

    /// Back up each config file that still sets a retired or deprecated key,
    /// migrating translated review settings before removal. Refuse legacy
    /// after-landing policies before any write. A file with none is untouched.
    fn remove_retired_config_keys(&self) -> Result<super::RetiredConfigKeyRepair, OrbitError>;
}

impl DoctorCommands for OrbitRuntime {
    fn repair_state_directory_permissions(&self) -> Result<usize, OrbitError> {
        super::permission::repair_state_directory_permissions(self)
    }
    fn doctor_workspace_with_depth(
        &self,
        env: &orbit_common::Env,
        deep: bool,
        host: &dyn Host,
    ) -> Result<Vec<WorkspaceDoctorResult>, OrbitError> {
        Ok(WORKSPACE_PROBES
            .iter()
            .flat_map(|probe| probe.run(self, env, deep, host))
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
        super::remove_stale_definition_artifacts(self)
    }

    fn remove_orphan_task_stores(
        &self,
        host: &dyn Host,
    ) -> Result<OrphanTaskStoreRemoval, OrbitError> {
        host.remove_unclaimed_task_stores(&self.global_root())
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

    fn remove_retired_config_keys(&self) -> Result<super::RetiredConfigKeyRepair, OrbitError> {
        super::retired_config::remove_retired_config_keys(self)
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

/// The workspace-level doctor rows of a kernel a kernel interface handed
/// back, such as the one a workspace catalog's attention read receives.
pub fn kernel_workspace_doctor(
    kernel: &crate::runtime::RuntimeKernel,
    env: &Env,
    deep: bool,
    host: &dyn Host,
) -> Result<Vec<WorkspaceDoctorResult>, OrbitError> {
    OrbitRuntime::from_kernel(kernel.clone()).doctor_workspace_with_depth(env, deep, host)
}
