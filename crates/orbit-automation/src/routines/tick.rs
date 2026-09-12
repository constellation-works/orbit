//! The stateless scheduler tick [ORB-10021, ORB-12237]: what runs when the OS
//! clock invokes `orbit clock tick` (`orbit sweep` is a compatibility alias).
//! One pass never bootstraps a workspace from the caller's cwd, isolates
//! per-routine failures into report rows, and returns `Err` only for
//! infrastructure failures (registry unreadable, store unopenable) — an
//! unconfigured host is a clean no-op, because launchd/systemd will invoke
//! this forever.
//!
//! This module is the host assembly around the deterministic policy in
//! [`super::sweep`]: it owns the pass lock, the scheduler store, workspace
//! discovery, dispatch, and the auto-task phase.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::observability::log_rotation::{self, LogRotationConfig};
use orbit_types::workflow::JobRunState;
use orbit_types::workspace::Workspace;

use super::RoutineHostIdentity;
use super::clock::load_clock_settings;
use super::loader::{
    DiscoveredWorkspaces, LoadedRoutine, RoutineLoadError, RoutineWorkspaceProvider,
    collect_workspace_routines,
};
use super::sweep::{
    AutoTaskSweepReport, RoutineDispatch, SweepOptions, SweepOutcome, run_sweep_core,
};
use crate::auto_tasks::scheduler::{SchedulerOptions, run_auto_task_scheduler_at};
use crate::consumers;
use crate::host::{AutomationHost, RunOwnerLiveness};

/// Production dispatch over the per-workspace hosts discovered this pass.
struct HostDispatch<'a, H> {
    hosts: BTreeMap<PathBuf, &'a H>,
}

impl<H: AutomationHost> RoutineDispatch for HostDispatch<'_, H> {
    fn evaluate_delivery(
        &self,
        routine: &LoadedRoutine,
        dry_run: bool,
        now: chrono::DateTime<Utc>,
    ) -> Result<orbit_types::workflow::automation::AutomationDiagnostic, OrbitError> {
        let host = self
            .hosts
            .get(&routine.source_orbit_dir)
            .ok_or_else(|| OrbitError::Execution("routine source missing".into()))?;
        consumers::evaluate_routine(*host, &routine.definition, dry_run, now)
    }

    fn submit(
        &self,
        source_orbit_dir: &Path,
        job_name: &str,
        actor: &str,
        slot: &str,
    ) -> Result<String, OrbitError> {
        let host = self.hosts.get(source_orbit_dir).ok_or_else(|| {
            OrbitError::WorkspaceError(format!(
                "no runtime for source workspace '{}'",
                source_orbit_dir.display()
            ))
        })?;
        host.submit_routine_run(source_orbit_dir, job_name, actor, slot)
    }

    fn run_state(&self, source_orbit_dir: &Path, run_id: &str) -> Option<JobRunState> {
        self.hosts
            .get(source_orbit_dir)
            .and_then(|host| host.show_job_run(run_id).ok())
            .map(|run| run.state)
    }

    fn run_owner_liveness(&self, source_orbit_dir: &Path, run_id: &str) -> RunOwnerLiveness {
        self.hosts
            .get(source_orbit_dir)
            // An unreadable run is not evidence that its worker stopped.
            .and_then(|host| {
                host.show_job_run(run_id)
                    .ok()
                    .map(|run| host.run_owner_liveness(&run))
            })
            .unwrap_or(RunOwnerLiveness::Unknown)
    }
}

/// Refresh each discovered workspace's read-side token projection once per
/// successful sweep. A stale projection must not stop routine evaluation.
pub fn refresh_discovered_token_scoreboards<H: AutomationHost>(workspaces: &[(Workspace, H)]) {
    for (workspace, host) in workspaces {
        if let Err(error) = host.refresh_token_scoreboard() {
            // Log targets are an observed surface, so they keep the names they
            // were emitted under before this pass moved crates [ORB-12262].
            tracing::warn!(
                target: "orbit.core.scoreboard",
                workspace = %workspace.name,
                error = %error,
                "failed to refresh tokens scoreboard during routine sweep",
            );
        }
    }
}

/// Run one sweep pass against `global_root` with a caller-supplied workspace
/// provider, rotating the host sweep log first.
pub fn run_sweep_with_providers<P: RoutineWorkspaceProvider>(
    global_root: &Path,
    options: SweepOptions,
    local_host: RoutineHostIdentity,
    workspace_provider: &P,
) -> Result<SweepOutcome, OrbitError> {
    // The OS clock invokes this every minute forever; on macOS launchd
    // redirects stdout/stderr into `logs/sweep.log`. Opportunistically roll +
    // prune it here (rename-based, best-effort) so an always-on host cannot
    // grow it without bound. No-op until the file exceeds the configured
    // per-file budget. `run_sweep_at_with_providers` (the explicit root seam)
    // is left untouched so tests never rotate real logs.
    log_rotation::rotate_and_prune(
        &super::clock::sweep_log_path(global_root),
        &LogRotationConfig::load_global_best_effort(),
    );
    run_sweep_at_with_providers(global_root, options, local_host, workspace_provider)
}

/// Run one sweep pass against an explicit global root using injected
/// composition. Provider calls occur only after the sweep lock is held.
pub fn run_sweep_at_with_providers<P: RoutineWorkspaceProvider>(
    global_root: &Path,
    options: SweepOptions,
    local_host: RoutineHostIdentity,
    workspace_provider: &P,
) -> Result<SweepOutcome, OrbitError> {
    run_sweep_at_with_providers_at(
        global_root,
        options,
        local_host,
        workspace_provider,
        Utc::now(),
    )
}

/// One scheduler tick at an explicit instant — the seam deterministic sweep
/// tests drive.
pub fn run_sweep_at_with_providers_at<P: RoutineWorkspaceProvider>(
    global_root: &Path,
    options: SweepOptions,
    local_host: RoutineHostIdentity,
    workspace_provider: &P,
    now_utc: chrono::DateTime<Utc>,
) -> Result<SweepOutcome, OrbitError> {
    // One pass per host at a time: overlapping invocations from a slow prior
    // pass must not double-fire. flock releases on process death, so a
    // crashed sweep never wedges the next one.
    let lock = orbit_store::try_acquire_routine_sweep_lock(&global_root.join("state"))?;
    let Some(_lock) = lock else {
        return Ok(SweepOutcome {
            host_id: local_host.host_id,
            machine_id: local_host.machine_id,
            lock_busy: true,
            ..SweepOutcome::default()
        });
    };

    let store = super::open_routine_store(global_root)?;
    // The OS unit and due calculation share this host-local setting. A
    // configured five-minute clock therefore keeps a slot natural for two
    // five-minute intervals instead of retaining the old 120-second default.
    let options = configured_sweep_options(global_root, options)?;
    // One host per active workspace; discovery and dispatch share them.
    let discovered = workspace_provider.discover_workspaces(global_root)?;
    refresh_discovered_token_scoreboards(&discovered.entries);
    let mut load_errors: Vec<RoutineLoadError> = discovered.errors.clone();
    let no_workspace_loaded = no_workspace_loaded_row(&discovered);

    let mut collection = collect_workspace_routines(&discovered.entries);
    load_errors.append(&mut collection.errors);

    let dispatch = HostDispatch {
        hosts: discovered
            .entries
            .iter()
            .map(|(_, host)| (host.shared_orbit_dir(), host))
            .collect(),
    };

    let reports = run_sweep_core(store.as_ref(), &collection, &dispatch, options, now_utc)?;
    // Auto-tasks run second so their task-store writes cannot delay routine
    // dispatch. The phase is bounded by this pass's discovered workspaces and
    // each scheduler's finite definition collection. A workspace-level error
    // becomes one row and never prevents the remaining workspaces from running.
    let mut auto_task_reports = Vec::new();
    for (workspace, host) in &discovered.entries {
        match run_auto_task_scheduler_at(
            host,
            now_utc,
            SchedulerOptions {
                dry_run: options.dry_run,
            },
        ) {
            Ok(outcome) => {
                auto_task_reports.extend(outcome.reports.into_iter().map(|report| {
                    AutoTaskSweepReport {
                        name: report.name,
                        source: workspace.name.clone(),
                        action: if report.action == "fired" {
                            "minted"
                        } else {
                            report.action
                        },
                        reason: report.reason,
                        slot: report.slot,
                        task_id: report.task_id,
                    }
                }));
                auto_task_reports.extend(outcome.errors.into_iter().map(|error| {
                    AutoTaskSweepReport {
                        name: error
                            .path
                            .as_ref()
                            .and_then(|path| path.file_stem())
                            .and_then(|name| name.to_str())
                            .unwrap_or("auto-task")
                            .to_string(),
                        source: workspace.name.clone(),
                        action: "error",
                        reason: Some(error.message),
                        slot: None,
                        task_id: None,
                    }
                }));
            }
            Err(error) => auto_task_reports.push(AutoTaskSweepReport {
                name: "auto-tasks".to_string(),
                source: workspace.name.clone(),
                action: "error",
                reason: Some(error.to_string()),
                slot: None,
                task_id: None,
            }),
        }
    }

    Ok(SweepOutcome {
        host_id: local_host.host_id,
        machine_id: local_host.machine_id,
        lock_busy: false,
        reports,
        auto_task_reports,
        load_errors,
        no_workspace_loaded,
    })
}

/// One fail-loud row when discovery found workspaces but opened none.
fn no_workspace_loaded_row<H>(discovered: &DiscoveredWorkspaces<H>) -> Option<String> {
    if !discovered.entries.is_empty() || discovered.errors.is_empty() {
        return None;
    }
    let first = &discovered.errors[0];
    let binary_version = env!("CARGO_PKG_VERSION");
    tracing::error!(
        target: "orbit.core.sweep",
        binary_version,
        source_workspace = first.source_workspace.as_str(),
        first_error = first.message.as_str(),
        "sweep.no_workspace_loaded"
    );
    Some(format!(
        "sweep.no_workspace_loaded: orbit {binary_version} loaded 0/{} workspaces; first error [{}]: {}",
        discovered.errors.len(),
        first.source_workspace,
        first.message
    ))
}

/// Bind a routine sweep to the same cadence the host clock installer renders.
/// Kept separate so the production path and its configuration test share one
/// explicit boundary.
pub fn configured_sweep_options(
    global_root: &Path,
    options: SweepOptions,
) -> Result<SweepOptions, OrbitError> {
    Ok(SweepOptions {
        sweep_cadence_seconds: load_clock_settings(global_root)?.cadence_seconds,
        ..options
    })
}
