//! Changing the sweep cadence. The settings themselves live in
//! `crate::clock_settings`; these re-exports keep the `routines::clock`
//! paths for this module's siblings and outer consumers.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::path::home_dir;

pub use crate::clock_settings::{
    ClockSettings, DEFAULT_CLOCK_CADENCE_SECONDS, LAUNCHD_LABEL, SYSTEMD_UNIT, load_clock_settings,
    save_clock_settings,
};

use super::enable::observe_clock_enabled_for_pause;
use super::install::{
    ClockInstallReport, ensure_clock_env_file, install_clock_with, launchd_plist_path,
    systemd_timer_path, write_launchd_unit, write_systemd_units,
};
use super::manager::{ClockCommandRunner, ClockPlatform, NativeClockCommandRunner};

/// Change cadence transactionally from the operator's perspective: the
/// persisted setting is restored if the reloaded native unit cannot activate.
/// A paused clock stays paused: its units are rewritten and nothing activates.
pub fn set_clock_cadence(
    global_root: &Path,
    cadence_seconds: u64,
) -> Result<ClockInstallReport, OrbitError> {
    let orbit_bin = std::env::current_exe()
        .map_err(|error| OrbitError::Io(format!("resolve current orbit executable: {error}")))?
        .to_string_lossy()
        .to_string();
    set_clock_cadence_with(
        global_root,
        cadence_seconds,
        &orbit_bin,
        ClockPlatform::current(),
        &NativeClockCommandRunner,
        &home_dir()?,
    )
}

pub(super) fn set_clock_cadence_with(
    global_root: &Path,
    cadence_seconds: u64,
    orbit_bin: &str,
    platform: ClockPlatform,
    runner: &dyn ClockCommandRunner,
    home: &Path,
) -> Result<ClockInstallReport, OrbitError> {
    let previous = load_clock_settings(global_root)?;
    // Observe before saving: an unreadable manager must refuse the change
    // rather than guess, because activating would resume a paused clock.
    let paused = clock_is_paused(platform, runner, home)?;
    save_clock_settings(global_root, ClockSettings { cadence_seconds })?;
    if paused {
        return match rewrite_paused_units(
            global_root,
            orbit_bin,
            ClockSettings { cadence_seconds },
            platform,
            home,
        ) {
            Ok(report) => Ok(report),
            Err(error) => {
                save_clock_settings(global_root, previous)?;
                let _ = rewrite_paused_units(global_root, orbit_bin, previous, platform, home);
                Err(error)
            }
        };
    }
    match install_clock_with(
        global_root,
        orbit_bin,
        ClockSettings { cadence_seconds },
        platform,
        runner,
        home,
    ) {
        Ok(report) if report.activated => Ok(report),
        Ok(report) => {
            save_clock_settings(global_root, previous)?;
            let rollback =
                install_clock_with(global_root, orbit_bin, previous, platform, runner, home)?;
            Err(OrbitError::Execution(format!(
                "clock update was not activated; restored the previous configured cadence and {} the previous unit; recovery: {}",
                if rollback.activated {
                    "reactivated"
                } else {
                    "could not reactivate"
                },
                report.manual_steps.join("; "),
            )))
        }
        Err(error) => {
            save_clock_settings(global_root, previous)?;
            match install_clock_with(global_root, orbit_bin, previous, platform, runner, home) {
                Ok(rollback) => Err(OrbitError::Execution(format!(
                    "clock update failed: {error}; restored the previous configured cadence and {} the previous unit{}",
                    if rollback.activated {
                        "reactivated"
                    } else {
                        "could not reactivate"
                    },
                    if rollback.manual_steps.is_empty() {
                        String::new()
                    } else {
                        format!("; recovery: {}", rollback.manual_steps.join("; "))
                    }
                ))),
                Err(rollback_error) => Err(OrbitError::Execution(format!(
                    "clock update failed: {error}; restored the previous configured cadence but could not restore its native unit: {rollback_error}"
                ))),
            }
        }
    }
}

/// Whether the installed clock is paused: its unit file exists but the manager
/// has it disabled (systemd) or unloaded (launchd). A clock that was never
/// installed is not paused; setting its cadence installs and activates it.
fn clock_is_paused(
    platform: ClockPlatform,
    runner: &dyn ClockCommandRunner,
    home: &Path,
) -> Result<bool, OrbitError> {
    let (enabled, _) = observe_clock_enabled_for_pause(platform, runner)?;
    if enabled {
        return Ok(false);
    }
    Ok(match platform {
        ClockPlatform::Launchd => launchd_plist_path(home).exists(),
        ClockPlatform::Systemd => systemd_timer_path(home).exists(),
    })
}

/// Rewrite the units for a paused clock without touching the manager, so the
/// new cadence takes effect on the next enable.
fn rewrite_paused_units(
    global_root: &Path,
    orbit_bin: &str,
    settings: ClockSettings,
    platform: ClockPlatform,
    home: &Path,
) -> Result<ClockInstallReport, OrbitError> {
    let files_written = match platform {
        ClockPlatform::Launchd => {
            vec![write_launchd_unit(global_root, orbit_bin, settings, home)?]
        }
        ClockPlatform::Systemd => write_systemd_units(orbit_bin, settings, home)?,
    };
    let (env_file, env_file_created) = ensure_clock_env_file(global_root)?;
    Ok(ClockInstallReport {
        files_written,
        activated: false,
        paused: true,
        manual_steps: Vec::new(),
        env_file,
        env_file_created,
    })
}
