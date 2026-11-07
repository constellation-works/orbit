//! Host-local clock settings: `clock.toml` and the sweep cadence. `automation`
//! reads the cadence for a condition hold's continuity limit, and `routines`
//! owns the clock that installs it, so the settings sit below both
//! [ORB-22623]; `orbit-core` re-exports them from `application::contracts`
//! [ORB-22688]. The file is the only state here: no runtime access, and the
//! caller names the global root.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use serde::{Deserialize, Serialize};

const CLOCK_SETTINGS_FILE: &str = "clock.toml";

/// launchd agent label (macOS).
pub const LAUNCHD_LABEL: &str = "com.orbit.sweep";
/// systemd unit base name (Linux).
pub const SYSTEMD_UNIT: &str = "orbit-sweep";
/// Default interval between sweep ticks, in seconds.
pub const DEFAULT_CLOCK_CADENCE_SECONDS: u64 = 60;
const MIN_CLOCK_CADENCE_SECONDS: u64 = 60;
const MAX_CLOCK_CADENCE_SECONDS: u64 = 3_600;

/// Host-local settings for the OS clock. This deliberately lives beside the
/// host database rather than in a workspace config: every registered workspace
/// shares one clock.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub struct ClockSettings {
    /// Interval between sweep ticks, in whole minutes expressed as seconds.
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
    /// Require a whole-minute cadence between 60 and 3,600 seconds inclusive.
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

/// Read and validate `clock.toml` beneath the existing global root, using
/// defaults when the file is absent. Reject settings that resolve outside it.
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

/// Validate and atomically write `clock.toml` beneath the existing global root.
/// Reject settings paths that resolve outside it.
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
