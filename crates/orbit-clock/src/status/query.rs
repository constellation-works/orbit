//! Native manager query entry points and command builders.

use std::path::Path;

use orbit_common::OrbitError;

use crate::manager::{ClockCommandRunner, ClockPlatform, ManagerCommand, NativeClockCommandRunner};
use crate::settings::{ClockSettings, LAUNCHD_LABEL, SYSTEMD_UNIT, load_clock_settings};

use super::{ClockStatus, SystemdClockDetails, launchd, manager_error, systemd};

pub(crate) fn manager_status_command(platform: ClockPlatform) -> ManagerCommand {
    match platform {
        ClockPlatform::Launchd => ManagerCommand {
            program: "launchctl",
            args: vec!["list".into(), LAUNCHD_LABEL.into()],
        },
        ClockPlatform::Systemd => ManagerCommand {
            program: "systemctl",
            args: vec![
                "--user".into(),
                "is-enabled".into(),
                format!("{SYSTEMD_UNIT}.timer"),
            ],
        },
    }
}

pub(crate) fn launchd_manager_probe_command() -> ManagerCommand {
    ManagerCommand {
        program: "launchctl",
        args: vec!["list".into()],
    }
}

/// `launchctl list <label>` only says the agent is loaded. The per-service
/// dump is what reveals a loaded agent that can no longer run.
pub(super) fn launchd_print_command(uid: u32) -> ManagerCommand {
    ManagerCommand {
        program: "launchctl",
        args: vec!["print".into(), format!("gui/{uid}/{LAUNCHD_LABEL}")],
    }
}

pub(super) fn systemd_next_trigger_command() -> ManagerCommand {
    ManagerCommand {
        program: "systemctl",
        args: vec![
            "--user".into(),
            "show".into(),
            format!("{SYSTEMD_UNIT}.timer"),
            "--property=LoadState".into(),
            "--property=ActiveState".into(),
            "--property=NextElapseUSecRealtime".into(),
            "--property=NextElapseUSecMonotonic".into(),
            "--property=LastTriggerUSec".into(),
        ],
    }
}

/// Read host clock settings and query the native manager for scheduling health.
pub fn clock_status(global_root: &Path) -> Result<ClockStatus, OrbitError> {
    let platform = ClockPlatform::current();
    // Resolved only on macOS: a systemd host has no launchd agent to inspect,
    // and must not start failing `clock status` because HOME is unset.
    let launchd = match platform {
        ClockPlatform::Launchd => Some(launchd::LaunchdHealthProbe::current()?),
        ClockPlatform::Systemd => None,
    };
    super::clock_status_with(
        global_root,
        platform,
        &NativeClockCommandRunner,
        launchd.as_ref(),
        systemd::monotonic_usec(),
    )
}

pub(crate) fn clock_status_with(
    global_root: &Path,
    platform: ClockPlatform,
    runner: &dyn ClockCommandRunner,
    launchd: Option<&launchd::LaunchdHealthProbe>,
    now_usec: Option<u64>,
) -> Result<ClockStatus, OrbitError> {
    let settings = load_clock_settings(global_root)?;
    let status_command = manager_status_command(platform);
    let status_output = runner.probe(&status_command).map_err(|error| {
        manager_error::clock_manager_probe_error(platform, &status_command, &error)
    })?;
    // A `launchctl print` dump captured while deciding `enabled`, so the
    // health check below does not ask launchd twice.
    let mut launchd_dump = None;
    let enabled = match platform {
        ClockPlatform::Launchd if !status_output.success => {
            if launchd::launchd_reports_not_loaded(&status_output) {
                false
            } else {
                let (loaded, dump) = launchd::launchd_loaded_without_list(
                    runner,
                    launchd,
                    &status_command,
                    &status_output,
                )?;
                launchd_dump = dump;
                loaded
            }
        }
        _ => status_output.success,
    };
    let mut manager_details = None;
    let mut enabled_unschedulable_warning = false;
    let (mut schedulable, mut health_issue) = if platform == ClockPlatform::Systemd {
        match systemd::query_systemd_clock_details(runner) {
            Ok(details) => {
                if !enabled
                    && (!systemd::systemd_reports_disabled_or_missing(&status_output)
                        || details.running.is_none())
                {
                    return Err(manager_error::clock_manager_unavailable_error(
                        platform,
                        &[(&status_command, &status_output)],
                        None,
                    ));
                }
                let schedulable = details.is_schedulable();
                let disabled_running = !enabled && details.is_running();
                manager_details = Some(details);
                if disabled_running {
                    (
                        schedulable,
                        Some("systemd timer is disabled but still active; run `orbit clock pause` to stop scheduled sweeps".to_string()),
                    )
                } else if !enabled {
                    (false, None)
                } else if schedulable {
                    (true, None)
                } else {
                    enabled_unschedulable_warning = true;
                    (
                        false,
                        Some(
                            "systemd timer is enabled but is not active with a finite future trigger; recovery: `orbit clock enable` rewrites a stale installed unit if needed, re-arms it, and verifies the result"
                                .to_string(),
                        ),
                    )
                }
            }
            Err(error) if enabled => (
                false,
                Some(format!(
                    "systemd timer is enabled but its next trigger could not be verified ({error}); recovery: inspect `systemctl --user status orbit-sweep.timer`, then run `orbit clock enable` to rewrite a stale unit if needed, re-arm, and verify it"
                )),
            ),
            Err(_) if systemd::systemd_reports_missing(&status_output) => (false, None),
            Err(error) => {
                return Err(manager_error::clock_manager_unavailable_error(
                    platform,
                    &[(&status_command, &status_output)],
                    Some(&error),
                ));
            }
        }
    } else if enabled {
        match launchd.and_then(|probe| launchd::launchd_health_issue(probe, runner, launchd_dump)) {
            Some(issue) => (false, Some(issue)),
            None => (true, None),
        }
    } else {
        (false, None)
    };
    if platform == ClockPlatform::Systemd && (enabled || schedulable) {
        let (service_schedulable, service_issue) =
            systemd::systemd_schedulability_with_health(runner, manager_details.as_ref(), now_usec);
        if service_schedulable {
            schedulable = true;
            if enabled_unschedulable_warning {
                health_issue = None;
            }
        }
        if let Some(issue) = service_issue {
            schedulable = false;
            health_issue = Some(match health_issue {
                Some(previous) => format!("{previous}; {issue}"),
                None => issue,
            });
        }
    }
    Ok(clock_status_from(
        settings,
        enabled,
        schedulable,
        platform,
        health_issue,
        manager_details,
    ))
}

pub(crate) fn clock_status_from(
    settings: ClockSettings,
    enabled: bool,
    schedulable: bool,
    platform: ClockPlatform,
    health_issue: Option<String>,
    manager_details: Option<SystemdClockDetails>,
) -> ClockStatus {
    let details = manager_details.unwrap_or_default();
    ClockStatus {
        configured_cadence_seconds: settings.cadence_seconds,
        effective_cadence_seconds: schedulable.then_some(settings.cadence_seconds),
        enabled,
        loaded: details.loaded.unwrap_or(enabled),
        running: details.running,
        schedulable,
        health_issue,
        last_tick_at: details.last_tick_at,
        next_tick_at: schedulable.then_some(details.next_tick_at).flatten(),
        platform: platform.name(),
    }
}
