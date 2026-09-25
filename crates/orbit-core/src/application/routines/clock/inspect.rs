//! Inspect the installed OS sweep-clock unit, compare it to this binary, and
//! repair it when the two have drifted apart.
//!
//! `orbit doctor` and `orbit clock status` share the inspection helper so a
//! launchd/systemd unit that still points at an older package-manager install
//! is visible without talking to the unit manager. [`converge_clock_unit`] is
//! the repair half: an installed unit whose program has moved or been deleted
//! stops firing silently, so `orbit update` and `orbit clock repair` rewrite it
//! to the running binary instead of waiting for an operator to notice.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use orbit_common::OrbitError;

use super::manager::ClockPlatform;
use super::program::{
    discover_clock_unit_program, display_opt_path, first_line, normalize_version, same_program,
};

/// How long to wait for `<program> --version` before treating it as unrunnable.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The binary this process is, used as the comparison baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningBinary {
    /// `std::env::current_exe()`, not necessarily canonical.
    pub path: PathBuf,
    /// `CARGO_PKG_VERSION` of this crate / workspace.
    pub version: String,
}

impl RunningBinary {
    /// Snapshot the running Orbit process.
    pub fn current() -> Result<Self, OrbitError> {
        let path = std::env::current_exe().map_err(|error| {
            OrbitError::Io(format!("resolve current orbit executable: {error}"))
        })?;
        Ok(Self {
            path,
            version: env!("CARGO_PKG_VERSION").to_string(),
        })
    }
}

/// Outcome of comparing the installed clock unit to this binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockUnitVerdict {
    /// No launchd plist or systemd service is present.
    NoUnitInstalled,
    /// Canonical path and version both match.
    Matching,
    /// Two different installs report the same version.
    PathMismatch,
    /// The unit's program reports a different version than this binary.
    VersionMismatch,
    /// The unit runs this binary but still uses the legacy `orbit sweep`
    /// invocation instead of `orbit clock tick`.
    InvocationMismatch,
    /// The unit exists but its program could not be probed.
    Unrunnable {
        /// Why `--version` did not yield a version string.
        reason: String,
    },
}

/// Facts about the installed clock unit relative to this binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockUnitInspection {
    /// Unit file that named the program, when one was found.
    pub unit_path: Option<PathBuf>,
    /// Program path as written in the unit, when parsed.
    pub program_path: Option<PathBuf>,
    /// Version reported by the unit program, when `--version` succeeded.
    pub program_version: Option<String>,
    /// Path of the running binary.
    pub running_path: PathBuf,
    /// Version of the running binary.
    pub running_version: String,
    /// Comparison result.
    pub verdict: ClockUnitVerdict,
}

impl ClockUnitInspection {
    /// Fragment appended after `platform:` on `orbit clock status`.
    pub fn status_line_suffix(&self) -> String {
        let Some(program_path) = &self.program_path else {
            return String::new();
        };
        let program = program_path.display();
        match &self.verdict {
            ClockUnitVerdict::NoUnitInstalled => String::new(),
            ClockUnitVerdict::Matching | ClockUnitVerdict::PathMismatch => {
                let version = self
                    .program_version
                    .as_deref()
                    .unwrap_or(&self.running_version);
                format!(" | program: {program} {version}")
            }
            ClockUnitVerdict::VersionMismatch => {
                let unit_version = self.program_version.as_deref().unwrap_or("unknown");
                format!(
                    " | program: {program} {unit_version} (mismatch: running {} at {})",
                    self.running_version,
                    self.running_path.display()
                )
            }
            ClockUnitVerdict::InvocationMismatch => format!(
                " | program: {program} (stale: invokes `orbit sweep`; run `orbit clock repair` to rewrite)"
            ),
            ClockUnitVerdict::Unrunnable { reason } => {
                format!(" | program: {program} (version unavailable: {reason})")
            }
        }
    }

    /// Operator-facing doctor detail for this inspection.
    pub fn doctor_message(&self) -> String {
        match &self.verdict {
            ClockUnitVerdict::NoUnitInstalled => {
                "no sweep clock unit is installed on this host".to_string()
            }
            ClockUnitVerdict::Matching => format!(
                "clock unit {} runs this binary ({}, {})",
                display_opt_path(&self.unit_path),
                self.running_path.display(),
                self.running_version
            ),
            ClockUnitVerdict::PathMismatch => format!(
                "clock unit {} runs {} ({}); this binary is {} (same version). Two installs; the clock can drift",
                display_opt_path(&self.unit_path),
                display_opt_path(&self.program_path),
                self.program_version
                    .as_deref()
                    .unwrap_or(&self.running_version),
                self.running_path.display()
            ),
            ClockUnitVerdict::VersionMismatch => format!(
                "clock unit {} runs {} ({}); this binary is {} ({})",
                display_opt_path(&self.unit_path),
                display_opt_path(&self.program_path),
                self.program_version.as_deref().unwrap_or("unknown"),
                self.running_path.display(),
                self.running_version
            ),
            ClockUnitVerdict::InvocationMismatch => format!(
                "clock unit {} runs {} through stale `orbit sweep`; the current unit invokes `orbit clock tick`",
                display_opt_path(&self.unit_path),
                display_opt_path(&self.program_path)
            ),
            ClockUnitVerdict::Unrunnable { reason } => format!(
                "clock unit {} names {}, which could not report a version: {reason}",
                display_opt_path(&self.unit_path),
                display_opt_path(&self.program_path)
            ),
        }
    }

    /// Repair hint for warning and failure rows.
    pub fn doctor_remediation(&self) -> Option<String> {
        match self.verdict {
            ClockUnitVerdict::VersionMismatch | ClockUnitVerdict::PathMismatch => Some(
                "Run `orbit clock repair` so the clock unit invokes this binary, or repoint the package-manager install the unit names so it is this version."
                    .to_string(),
            ),
            ClockUnitVerdict::InvocationMismatch => Some(
                "Run `orbit clock repair` to rewrite the stale unit to `orbit clock tick`."
                    .to_string(),
            ),
            ClockUnitVerdict::Unrunnable { .. } => Some(
                "Restore the orbit binary the clock unit names, or run `orbit clock repair` to rewrite the unit to this binary."
                    .to_string(),
            ),
            ClockUnitVerdict::NoUnitInstalled | ClockUnitVerdict::Matching => None,
        }
    }
}

/// Inspect the installed clock unit on this host against the running binary.
pub fn inspect_clock_unit() -> Result<ClockUnitInspection, OrbitError> {
    let home = orbit_common::fs::path::home_dir()?;
    let running = RunningBinary::current()?;
    Ok(inspect_clock_unit_at(
        &home,
        ClockPlatform::current(),
        &running,
        probe_program_version,
    ))
}

/// Inspect a clock unit under an explicit home and platform.
///
/// `probe` is injected so unit tests can cover matching and mismatch without
/// spawning; production passes [`probe_program_version`].
pub(crate) fn inspect_clock_unit_at(
    home: &Path,
    platform: ClockPlatform,
    running: &RunningBinary,
    probe: impl Fn(&Path) -> Result<String, String>,
) -> ClockUnitInspection {
    match discover_clock_unit_program(home, platform) {
        None => ClockUnitInspection {
            unit_path: None,
            program_path: None,
            program_version: None,
            running_path: running.path.clone(),
            running_version: running.version.clone(),
            verdict: ClockUnitVerdict::NoUnitInstalled,
        },
        Some(Err((unit_path, reason))) => ClockUnitInspection {
            unit_path: Some(unit_path),
            program_path: None,
            program_version: None,
            running_path: running.path.clone(),
            running_version: running.version.clone(),
            verdict: ClockUnitVerdict::Unrunnable { reason },
        },
        Some(Ok((unit_path, program_path, legacy_invocation))) => match probe(&program_path) {
            Err(reason) => ClockUnitInspection {
                unit_path: Some(unit_path),
                program_path: Some(program_path),
                program_version: None,
                running_path: running.path.clone(),
                running_version: running.version.clone(),
                verdict: ClockUnitVerdict::Unrunnable { reason },
            },
            Ok(raw_version) => {
                let program_version = normalize_version(&raw_version);
                let running_version = normalize_version(&running.version);
                let verdict = if legacy_invocation {
                    ClockUnitVerdict::InvocationMismatch
                } else if program_version != running_version {
                    ClockUnitVerdict::VersionMismatch
                } else if same_program(&program_path, &running.path) {
                    ClockUnitVerdict::Matching
                } else {
                    ClockUnitVerdict::PathMismatch
                };
                ClockUnitInspection {
                    unit_path: Some(unit_path),
                    program_path: Some(program_path),
                    program_version: Some(program_version),
                    running_path: running.path.clone(),
                    running_version: running.version.clone(),
                    verdict,
                }
            }
        },
    }
}

/// Run `<program> --version` with a short timeout. Never panics.
pub fn probe_program_version(program: &Path) -> Result<String, String> {
    probe_program_version_within(program, VERSION_PROBE_TIMEOUT)
}

/// [`probe_program_version`] with an explicit budget.
///
/// Production keeps the short [`VERSION_PROBE_TIMEOUT`] so a hung program
/// cannot stall `orbit doctor`; a test that spawns a real script under a
/// loaded runner passes a budget that only a genuinely hung process exceeds.
pub(crate) fn probe_program_version_within(
    program: &Path,
    timeout: Duration,
) -> Result<String, String> {
    if !program.exists() {
        return Err(format!("program does not exist: {}", program.display()));
    }

    let mut child = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", program.display()))?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_string(&mut stdout);
                }
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                if !status.success() {
                    let detail = first_line(&stderr).or_else(|| first_line(&stdout));
                    return Err(match detail {
                        Some(detail) => format!("exited {status}: {detail}"),
                        None => format!("exited {status}"),
                    });
                }
                let output = if stdout.trim().is_empty() {
                    stderr
                } else {
                    stdout
                };
                let version = normalize_version(&output);
                if version.is_empty() {
                    return Err("empty --version output".to_string());
                }
                return Ok(version);
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("--version timed out after {}s", timeout.as_secs()));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait for --version failed: {error}"));
            }
        }
    }
}
