use std::path::Path;

use clap::{Args, Subcommand};
use orbit_core::application::routines::{
    ClockHealth, ClockStatus, ClockUnitInspection, ClockUnitVerdict, clock_status,
    inspect_clock_unit, set_clock_cadence, set_clock_enabled,
};
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::{Value, json};

use super::ClockTickArgs;
use crate::command::{CommandOut, Payload};
use orbit_registry::workspace_registry;

#[derive(Args)]
#[command(
    about = "Inspect and control the host scheduler clock",
    arg_required_else_help = true,
    subcommand_required = true,
    after_help = "The host-wide OS clock invokes `orbit clock tick`; it does not change `orbit routine pause <name>` state. `orbit sweep` remains a compatibility alias for `orbit clock tick`."
)]
pub struct ClockCommand {
    #[command(subcommand)]
    pub command: ClockSubcommand,
}

#[derive(Subcommand)]
pub enum ClockSubcommand {
    /// Show configured cadence and native manager state
    Status,
    /// Disable scheduled ticks; manual `orbit clock tick` remains available
    Pause,
    /// Enable scheduled ticks using the configured cadence
    Enable,
    /// Rewrite the installed clock unit when it names a missing, moved, or stale program
    Repair,
    /// Persist a whole-minute cadence and reload the installed clock unit
    Set {
        #[arg(long)]
        cadence_seconds: u64,
    },
    /// Evaluate due routines and auto-tasks on this host
    Tick(ClockTickArgs),
}

impl ClockCommand {
    pub fn execute_without_runtime(
        self,
        root_override: Option<&Path>,
        workspace_selector: Option<&str>,
    ) -> CommandOut {
        match self.command {
            ClockSubcommand::Status => {
                let global_root = selected_global_root(root_override)?;
                let status = clock_status(&global_root)?;
                // A unit that cannot be inspected leaves the program fragment
                // out rather than failing the status report.
                let unit = inspect_clock_unit().ok();
                Ok(clock_status_payload(&status, unit.as_ref()).into())
            }
            ClockSubcommand::Pause => {
                let global_root = selected_global_root(root_override)?;
                let status = set_clock_enabled(&global_root, false)?;
                let text = format!(
                    "host scheduler clock paused ({}); manual `orbit clock tick` remains available",
                    status.platform
                );
                Ok(Payload::detail(
                    json!({
                        "action": "pause",
                        "enabled": status.enabled,
                        "manual_tick_available": true,
                        "platform": status.platform,
                    }),
                    text,
                )
                .into())
            }
            ClockSubcommand::Enable => {
                let global_root = selected_global_root(root_override)?;
                let status = set_clock_enabled(&global_root, true)?;
                let text = format!(
                    "host scheduler clock enabled: runs every {} seconds ({})",
                    status.configured_cadence_seconds, status.platform
                );
                Ok(Payload::detail(
                    json!({
                        "action": "enable",
                        "enabled": status.enabled,
                        "configured_cadence_seconds": status.configured_cadence_seconds,
                        "platform": status.platform,
                    }),
                    text,
                )
                .into())
            }
            ClockSubcommand::Repair => {
                super::repair::execute(&selected_global_root(root_override)?)
            }
            ClockSubcommand::Set { cadence_seconds } => {
                let global_root = selected_global_root(root_override)?;
                let report = set_clock_cadence(&global_root, cadence_seconds)?;
                let text = if report.paused {
                    format!(
                        "host scheduler clock cadence set to {cadence_seconds} seconds; the clock is paused, so the new cadence applies after `orbit clock enable`"
                    )
                } else {
                    format!(
                        "host scheduler clock cadence set to {cadence_seconds} seconds and reloaded"
                    )
                };
                Ok(Payload::detail(
                    json!({
                        "action": "set_cadence",
                        "configured_cadence_seconds": cadence_seconds,
                        "activated": report.activated,
                        "paused": report.paused,
                    }),
                    text,
                )
                .into())
            }
            ClockSubcommand::Tick(args) => {
                args.execute_without_runtime(root_override, workspace_selector)
            }
        }
    }
}

/// `orbit clock status` as a detail payload: one JSON object under
/// `--format json`/`ndjson`, the one-line summary (plus a health line when
/// the clock is unhealthy) on a terminal.
pub(super) fn clock_status_payload(
    status: &ClockStatus,
    unit: Option<&ClockUnitInspection>,
) -> Payload {
    Payload::detail(
        clock_status_doc(status, unit),
        clock_status_text(status, unit),
    )
}

/// The machine-readable record: every `ClockStatus` field, the derived
/// `state`, and the installed unit's program facts when inspection succeeded.
fn clock_status_doc(status: &ClockStatus, unit: Option<&ClockUnitInspection>) -> Value {
    json!({
        "state": clock_state(status),
        "enabled": status.enabled,
        "loaded": status.loaded,
        "running": status.running,
        "schedulable": status.schedulable,
        "configured_cadence_seconds": status.configured_cadence_seconds,
        "effective_cadence_seconds": status.effective_cadence_seconds,
        "platform": status.platform,
        "health_issue": status.health_issue,
        "last_tick_at": status.last_tick_at,
        "next_tick_at": status.next_tick_at,
        "program": unit.map(clock_program_doc),
    })
}

/// The human one-liner, followed by `clock health: …` when there is an issue.
fn clock_status_text(status: &ClockStatus, unit: Option<&ClockUnitInspection>) -> String {
    let mut text = format!(
        "clock: {} | configured cadence: {}s | effective cadence: {} | platform: {}{}",
        clock_state(status),
        status.configured_cadence_seconds,
        status
            .effective_cadence_seconds
            .map(|value| format!("{value}s"))
            .unwrap_or_else(|| "inactive".to_string()),
        status.platform,
        unit.map(ClockUnitInspection::status_line_suffix)
            .unwrap_or_default()
    );
    if let Some(issue) = &status.health_issue {
        text.push_str(&format!("\nclock health: {issue}"));
    }
    text
}

/// The `state` token: any reported issue, a running paused timer or an enabled
/// clock without a future trigger is `unhealthy`.
fn clock_state(status: &ClockStatus) -> &'static str {
    match status.health() {
        ClockHealth::Healthy => "enabled",
        ClockHealth::Paused => "paused",
        ClockHealth::ScheduledWithIssue
        | ClockHealth::Missed
        | ClockHealth::PausedWithIssue
        | ClockHealth::RunningWhilePaused => "unhealthy",
    }
}

fn clock_program_doc(unit: &ClockUnitInspection) -> Value {
    // Paths go through `display()`: a non-UTF-8 path must degrade, not fail
    // the whole document.
    let display = |path: &Path| path.display().to_string();
    let (verdict, reason) = match &unit.verdict {
        ClockUnitVerdict::NoUnitInstalled => ("no_unit_installed", None),
        ClockUnitVerdict::Matching => ("matching", None),
        ClockUnitVerdict::PathMismatch => ("path_mismatch", None),
        ClockUnitVerdict::VersionMismatch => ("version_mismatch", None),
        ClockUnitVerdict::InvocationMismatch => ("invocation_mismatch", None),
        ClockUnitVerdict::SafetyMismatch { reason } => ("safety_mismatch", Some(reason.as_str())),
        ClockUnitVerdict::Unrunnable { reason } => ("unrunnable", Some(reason.as_str())),
    };
    json!({
        "unit_path": unit.unit_path.as_deref().map(display),
        "path": unit.program_path.as_deref().map(display),
        "version": unit.program_version,
        "running_path": display(&unit.running_path),
        "running_version": unit.running_version,
        "verdict": verdict,
        "reason": reason,
    })
}

fn selected_global_root(root_override: Option<&Path>) -> Result<std::path::PathBuf, OrbitError> {
    let has_env_override = std::env::var("ORBIT_ROOT").is_ok_and(|root| !root.trim().is_empty());
    if root_override.is_some() || has_env_override {
        let cwd = std::env::current_dir().map_err(|error| OrbitError::Io(error.to_string()))?;
        return OrbitRuntime::resolve_roots_for_cwd(&cwd, root_override)
            .map(|roots| roots.global_root);
    }
    workspace_registry::global_orbit_dir()
}
