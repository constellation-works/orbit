//! Pause and resume the OS clock through its native manager.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::path::home_dir;

use super::converge::clear_clock_reload_pending;
use super::inspect::probe_program_version;
use super::install::{
    launchd_plist_path, migrate_stale_systemd_service, migrate_stale_systemd_timer,
    reload_launchd_unit, systemd_daemon_reload_command, systemd_enable_command,
    systemd_restart_command, write_launchd_unit,
};
use super::manager::{ClockCommandRunner, ClockPlatform, ManagerCommand, NativeClockCommandRunner};
use super::settings::{SYSTEMD_UNIT, load_clock_settings};
use super::status::{
    ClockStatus, LaunchdHealthProbe, clock_manager_probe_error, clock_manager_unavailable_error,
    clock_status_from, launchd_health_issue, launchd_manager_probe_command,
    launchd_reports_not_loaded, manager_command_error, manager_status_command,
    query_systemd_clock_details, systemd_reports_disabled_or_missing, systemd_reports_missing,
    systemd_unschedulable_error,
};

fn manager_set_enabled_command(
    platform: ClockPlatform,
    enabled: bool,
    home: &Path,
) -> ManagerCommand {
    match (platform, enabled) {
        (ClockPlatform::Launchd, true) => ManagerCommand {
            program: "launchctl",
            args: vec![
                "load".into(),
                launchd_plist_path(home).display().to_string(),
            ],
        },
        (ClockPlatform::Launchd, false) => ManagerCommand {
            program: "launchctl",
            args: vec![
                "unload".into(),
                launchd_plist_path(home).display().to_string(),
            ],
        },
        (ClockPlatform::Systemd, true) => systemd_enable_command(),
        (ClockPlatform::Systemd, false) => ManagerCommand {
            program: "systemctl",
            args: vec![
                "--user".into(),
                "disable".into(),
                "--now".into(),
                format!("{SYSTEMD_UNIT}.timer"),
            ],
        },
    }
}

/// Enable or pause the native per-user clock. This does not touch the routine
/// store, so manual `orbit clock tick` and per-routine pause state remain available.
pub fn set_clock_enabled(global_root: &Path, enabled: bool) -> Result<ClockStatus, OrbitError> {
    set_clock_enabled_with(
        global_root,
        enabled,
        ClockPlatform::current(),
        &NativeClockCommandRunner,
        &home_dir()?,
    )
}

pub(super) fn set_clock_enabled_with(
    global_root: &Path,
    enabled: bool,
    platform: ClockPlatform,
    runner: &dyn ClockCommandRunner,
    home: &Path,
) -> Result<ClockStatus, OrbitError> {
    set_clock_enabled_with_version_probe(
        global_root,
        enabled,
        platform,
        runner,
        home,
        probe_program_version,
    )
}

/// Inject the same version-probe seam as clock inspection, so manager fixtures
/// do not depend on executable startup time on the test host.
pub(super) fn set_clock_enabled_with_version_probe(
    global_root: &Path,
    enabled: bool,
    platform: ClockPlatform,
    runner: &dyn ClockCommandRunner,
    home: &Path,
    version_probe: fn(&Path) -> Result<String, String>,
) -> Result<ClockStatus, OrbitError> {
    let status = apply_clock_enabled(global_root, enabled, platform, runner, home, version_probe)?;
    // The operator just said what state the clock should be in; a reload a
    // failed repair left pending must not re-arm a clock paused after it.
    clear_clock_reload_pending(global_root)?;
    Ok(status)
}

fn apply_clock_enabled(
    global_root: &Path,
    enabled: bool,
    platform: ClockPlatform,
    runner: &dyn ClockCommandRunner,
    home: &Path,
    version_probe: fn(&Path) -> Result<String, String>,
) -> Result<ClockStatus, OrbitError> {
    let settings = load_clock_settings(global_root)?;
    let (current, missing) = if enabled {
        // Systemd always repairs and verifies below. A loaded launchd agent
        // also needs health inspection before enable can be an idempotent no-op.
        (
            runner
                .run(&manager_status_command(platform))
                .unwrap_or(false),
            false,
        )
    } else {
        observe_clock_enabled_for_pause(platform, runner)?
    };
    if platform == ClockPlatform::Systemd && enabled {
        migrate_stale_systemd_service(home)?;
        migrate_stale_systemd_timer(home, settings)?;
        let reload = systemd_daemon_reload_command();
        if !runner.run(&reload)? {
            return Err(manager_command_error(&reload));
        }
        let enable = systemd_enable_command();
        if !runner.run(&enable)? {
            return Err(manager_command_error(&enable));
        }
        // Starting an already-active elapsed timer is a no-op. Restarting
        // after daemon-reload makes `enable` repair a stale installed unit
        // as well as resume a paused clock.
        let restart = systemd_restart_command();
        if !runner.run(&restart)? {
            return Err(manager_command_error(&restart));
        }
        let details = query_systemd_clock_details(runner)?;
        if !details.is_schedulable() {
            return Err(systemd_unschedulable_error("enable completed"));
        }
        return Ok(clock_status_from(
            settings,
            true,
            true,
            platform,
            None,
            Some(details),
        ));
    }
    if platform == ClockPlatform::Systemd {
        if missing {
            return Ok(clock_status_from(
                settings, false, false, platform, None, None,
            ));
        }
        if !current {
            let details = query_systemd_clock_details(runner)?;
            if details.is_stopped() {
                return Ok(clock_status_from(
                    settings,
                    false,
                    false,
                    platform,
                    None,
                    Some(details),
                ));
            }
            if !details.is_running() {
                return Err(OrbitError::Execution(
                    "systemd timer activity could not be determined; inspect `systemctl --user status orbit-sweep.timer` before pausing".to_string(),
                ));
            }
        }
        let command = manager_set_enabled_command(platform, false, home);
        if !runner.run(&command)? {
            return Err(manager_command_error(&command));
        }
        let details = query_systemd_clock_details(runner)?;
        if !details.is_stopped() {
            return Err(OrbitError::Execution(
                "systemd timer pause completed, but the manager did not confirm an inactive timer without a future trigger; inspect `systemctl --user status orbit-sweep.timer`".to_string(),
            ));
        }
        return Ok(clock_status_from(
            settings,
            false,
            false,
            platform,
            None,
            Some(details),
        ));
    }
    if enabled && current {
        let mut probe = LaunchdHealthProbe::at(home)?;
        probe.version_probe = version_probe;
        if let Some(issue) = launchd_health_issue(&probe, runner, None) {
            tracing::debug!(%issue, "reloading unhealthy launchd clock");
            write_launchd_unit(
                global_root,
                &probe.running.path.to_string_lossy(),
                settings,
                home,
            )?;
            if !reload_launchd_unit(runner, home) {
                return Err(manager_command_error(&manager_set_enabled_command(
                    platform, true, home,
                )));
            }
        }
    }
    if current == enabled {
        return Ok(clock_status_from(
            settings, enabled, enabled, platform, None, None,
        ));
    }
    let command = manager_set_enabled_command(platform, enabled, home);
    let succeeded = runner.run(&command)?;
    if !succeeded {
        return Err(manager_command_error(&command));
    }
    Ok(clock_status_from(
        settings, enabled, enabled, platform, None, None,
    ))
}

/// Observe enough native-manager state to make pause safe and idempotent.
/// A failed status command is disabled or missing only when the manager's
/// diagnostic names that state; transport and ambiguous failures
/// must stop before the control path can mutate the manager.
fn observe_clock_enabled_for_pause(
    platform: ClockPlatform,
    runner: &dyn ClockCommandRunner,
) -> Result<(bool, bool), OrbitError> {
    let status_command = manager_status_command(platform);
    let status_output = runner
        .probe(&status_command)
        .map_err(|error| clock_manager_probe_error(platform, &status_command, &error))?;
    if status_output.success {
        return Ok((true, false));
    }

    match platform {
        ClockPlatform::Systemd if systemd_reports_disabled_or_missing(&status_output) => {
            Ok((false, systemd_reports_missing(&status_output)))
        }
        ClockPlatform::Systemd => Err(clock_manager_unavailable_error(
            platform,
            &[(&status_command, &status_output)],
            None,
        )),
        ClockPlatform::Launchd if launchd_reports_not_loaded(&status_output) => Ok((false, false)),
        ClockPlatform::Launchd => {
            let manager_command = launchd_manager_probe_command();
            let manager_output = runner
                .probe(&manager_command)
                .map_err(|error| clock_manager_probe_error(platform, &manager_command, &error))?;
            if manager_output.success {
                Ok((false, false))
            } else {
                Err(clock_manager_unavailable_error(
                    platform,
                    &[
                        (&status_command, &status_output),
                        (&manager_command, &manager_output),
                    ],
                    None,
                ))
            }
        }
    }
}
