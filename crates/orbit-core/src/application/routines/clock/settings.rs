//! Host-local clock settings: `clock.toml` and the sweep cadence.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_common::fs::path::home_dir;
use serde::{Deserialize, Serialize};

use super::install::{ClockInstallReport, install_clock_with};
use super::manager::{ClockCommandRunner, ClockPlatform, NativeClockCommandRunner};

const CLOCK_SETTINGS_FILE: &str = "clock.toml";

/// launchd agent label (macOS).
pub const LAUNCHD_LABEL: &str = "com.orbit.sweep";
/// systemd unit base name (Linux).
pub const SYSTEMD_UNIT: &str = "orbit-sweep";
pub const DEFAULT_CLOCK_CADENCE_SECONDS: u64 = 60;
const MIN_CLOCK_CADENCE_SECONDS: u64 = 60;
const MAX_CLOCK_CADENCE_SECONDS: u64 = 3_600;

/// Host-local settings for the OS clock. This deliberately lives beside the
/// host database rather than in a workspace config: every registered workspace
/// shares one clock.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub struct ClockSettings {
    pub cadence_seconds: u64,
}

impl Default for ClockSettings {
    fn default() -> Self {
        Self {
            cadence_seconds: DEFAULT_CLOCK_CADENCE_SECONDS,
        }
    }
}

impl ClockSettings {
    pub fn validate(self) -> Result<Self, OrbitError> {
        if !(MIN_CLOCK_CADENCE_SECONDS..=MAX_CLOCK_CADENCE_SECONDS).contains(&self.cadence_seconds)
            || !self.cadence_seconds.is_multiple_of(60)
        {
            return Err(OrbitError::InvalidInput(format!(
                "clock cadence_seconds must be a whole minute from {MIN_CLOCK_CADENCE_SECONDS} to {MAX_CLOCK_CADENCE_SECONDS} (got {})",
                self.cadence_seconds
            )));
        }
        Ok(self)
    }
}

pub fn clock_settings_path(global_root: &Path) -> PathBuf {
    global_root.join(CLOCK_SETTINGS_FILE)
}

pub fn load_clock_settings(global_root: &Path) -> Result<ClockSettings, OrbitError> {
    let path = validated_clock_settings_path(global_root)?;
    let raw = match fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ClockSettings::default());
        }
        Err(error) => {
            return Err(OrbitError::Io(format!("read {}: {error}", path.display())));
        }
    };
    toml::from_str::<ClockSettings>(&raw)
        .map_err(|error| {
            OrbitError::InvalidInput(format!(
                "invalid clock configuration {}: {error}",
                path.display()
            ))
        })?
        .validate()
}

pub fn save_clock_settings(global_root: &Path, settings: ClockSettings) -> Result<(), OrbitError> {
    let settings = settings.validate()?;
    let rendered = toml::to_string(&settings).map_err(|error| {
        OrbitError::Execution(format!("serialize clock configuration: {error}"))
    })?;
    let path = validated_clock_settings_path(global_root)?;
    atomic_write_text(&path, &rendered)
        .map_err(|error| OrbitError::Io(format!("write clock configuration: {error}")))
}

/// Resolve the clock settings file beneath the canonical global root.
///
/// The root may be selected through an explicit CLI override, but the clock
/// settings path itself is fixed. Canonicalizing both components and requiring
/// the exact expected file prevents a symlink or traversal from redirecting a
/// settings read to another host file.
fn validated_clock_settings_path(global_root: &Path) -> Result<PathBuf, OrbitError> {
    let canonical_root = fs::canonicalize(global_root).map_err(|error| {
        OrbitError::Io(format!(
            "resolve clock configuration root {}: {error}",
            global_root.display()
        ))
    })?;
    let expected_path = canonical_root.join(CLOCK_SETTINGS_FILE);

    let canonical_path = match fs::canonicalize(&expected_path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(expected_path);
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "resolve clock configuration {}: {error}",
                expected_path.display()
            )));
        }
    };
    if canonical_path != expected_path || !canonical_path.is_file() {
        return Err(OrbitError::InvalidInput(format!(
            "clock configuration must be a regular {CLOCK_SETTINGS_FILE} directly under {}",
            canonical_root.display()
        )));
    }

    Ok(canonical_path)
}

/// Change cadence transactionally from the operator's perspective: the
/// persisted setting is restored if the reloaded native unit cannot activate.
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
    save_clock_settings(global_root, ClockSettings { cadence_seconds })?;
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
