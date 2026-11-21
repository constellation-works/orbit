use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workspace::Workspace;

use super::super::clock::load_clock_settings;
use super::super::loader::{DiscoveredWorkspaces, RoutineLoadError};
use super::SweepOptions;
use crate::OrbitRuntime;
use crate::application::contracts::PullSettlementOutcome;
use crate::runtime::host_signal::{HostSignalProbe, ScheduledShutdown};

/// Load the `clock.env` values any discovered workspace's effective
/// `execution.env.pass` lists or a crew's `env` reads. A refused or
/// unreadable file is a load error row, not a failed tick: runs still start,
/// and a Claude activity without its token then fails fast with an
/// actionable error.
pub(super) fn load_clock_env_for(
    global_root: &Path,
    discovered: &mut DiscoveredWorkspaces,
    load_errors: &mut Vec<RoutineLoadError>,
) -> Vec<String> {
    let pass: BTreeSet<String> = discovered
        .entries
        .iter()
        .chain(&discovered.replicas)
        .flat_map(|(_, runtime)| runtime.child_env_default_names())
        .collect();
    let pass: Vec<String> = pass.into_iter().collect();
    match orbit_common::security::operator_env::load_clock_env(global_root, &pass) {
        Ok(defaults) => {
            for (_, runtime) in discovered
                .entries
                .iter_mut()
                .chain(&mut discovered.replicas)
            {
                runtime.context.apply_child_env_defaults(&defaults);
            }
            defaults.into_iter().map(|(name, _)| name).collect()
        }
        Err(error) => {
            let path = orbit_common::security::operator_env::clock_env_file_path(global_root);
            load_errors.push(RoutineLoadError {
                source_workspace: orbit_common::security::operator_env::CLOCK_ENV_FILE_NAME
                    .to_string(),
                path: Some(path),
                message: error.to_string(),
            });
            Vec::new()
        }
    }
}

pub(super) fn tick_allows_workspace(
    deadline: Instant,
    workspace: &Workspace,
    skipped: &mut BTreeSet<String>,
) -> bool {
    if Instant::now() < deadline {
        return true;
    }
    skipped.insert(workspace.name.clone());
    false
}

/// Run one sweep phase for each workspace while enforcing its deadline and
/// standardizing failures into the phase's existing warning event.
pub(super) fn each_workspace_in_tick<'a, T>(
    workspaces: impl Iterator<Item = &'a (Workspace, OrbitRuntime)>,
    deadline: Instant,
    skipped: &mut BTreeSet<String>,
    failed_event: Option<&'static str>,
    mut on_deadline: Option<&mut dyn FnMut(&Workspace)>,
    mut phase: impl FnMut(&Workspace, &OrbitRuntime) -> Result<T, OrbitError>,
) {
    for (workspace, runtime) in workspaces {
        if !tick_allows_workspace(deadline, workspace, skipped) {
            if let Some(on_deadline) = &mut on_deadline {
                on_deadline(workspace);
            }
            continue;
        }

        if let Err(error) = phase(workspace, runtime)
            && let Some(event) = failed_event
        {
            tracing::warn!(
                target: "orbit.core.sweep",
                workspace = %workspace.name,
                error = %error,
                "{}",
                event
            );
        }
    }
}

/// Deliver one checkout's recorded pull settlements, logging what is still
/// pending; the next tick retries it.
pub(super) fn deliver_recorded_pull_settlements(workspace: &Workspace, runtime: &OrbitRuntime) {
    for entry in runtime.deliver_recorded_pull_settlements() {
        match entry.outcome {
            PullSettlementOutcome::Settled | PullSettlementOutcome::ClosedObsolete => {
                tracing::info!(
                    target: "orbit.core.sweep",
                    workspace = %workspace.name,
                    settlement = %entry.describe(),
                    "sweep.pull_settlement_delivered",
                )
            }
            PullSettlementOutcome::PendingDelivery
            | PullSettlementOutcome::OwnerUnreachable
            | PullSettlementOutcome::Pending
            | PullSettlementOutcome::NoOwnerRoute => {
                tracing::warn!(
                    target: "orbit.core.sweep",
                    workspace = %workspace.name,
                    settlement = %entry.describe(),
                    "sweep.pull_settlement_pending",
                )
            }
            _ => {}
        }
    }
}

/// One fail-loud row when discovery found workspaces but opened none.
pub(super) fn no_workspace_loaded_row(discovered: &DiscoveredWorkspaces) -> Option<String> {
    if !discovered.entries.is_empty()
        || !discovered.replicas.is_empty()
        || discovered.errors.is_empty()
    {
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

/// The pending shutdown when it falls inside the fixed hold horizon
/// ([`crate::runtime::host_signal::HOST_SHUTDOWN_HOLD_MINUTES`]) of this tick.
pub(super) fn shutdown_holding_admission(
    host_signals: &dyn HostSignalProbe,
    now_utc: chrono::DateTime<Utc>,
) -> Option<ScheduledShutdown> {
    let shutdown = host_signals.scheduled_shutdown()?;
    shutdown
        .holds_admission(
            now_utc,
            crate::runtime::host_signal::HOST_SHUTDOWN_HOLD_MINUTES,
        )
        .then_some(shutdown)
}

/// Bind a routine sweep to the same cadence the host clock installer renders.
/// Kept separate so the production path and its configuration test share one
/// explicit boundary.
pub(super) fn configured_sweep_options(
    global_root: &Path,
    options: SweepOptions,
) -> Result<SweepOptions, OrbitError> {
    Ok(SweepOptions {
        sweep_cadence_seconds: load_clock_settings(global_root)?.cadence_seconds,
        ..options
    })
}
