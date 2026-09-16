//! Inspect the installed OS sweep-clock unit, compare it to this binary, and
//! repair it when the two have drifted apart.
//!
//! `orbit doctor` and `orbit clock status` share the inspection helper so a
//! launchd/systemd unit that still points at an older package-manager install
//! is visible without talking to the unit manager. [`converge_clock_unit`] is
//! the repair half: an installed unit whose program has moved or been deleted
//! stops firing silently, so `orbit update` and `orbit clock repair` rewrite it
//! to the running binary instead of waiting for an operator to notice.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;

use super::clock::{
    ClockCommandRunner, ClockPlatform, NativeClockCommandRunner, launchd_manual_steps,
    launchd_plist_path, load_clock_settings, manager_status_command, reload_launchd_unit,
    restart_systemd_unit, systemd_manual_steps, systemd_service_path, write_launchd_unit,
    write_systemd_units,
};

/// How long to wait for `<program> --version` before treating it as unrunnable.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Marker written next to `clock.toml` when a convergence pass rewrote a
/// registered unit but the manager would not re-register it.
///
/// `launchctl load` failing after the `unload` leaves the job unloaded, which
/// is exactly what a clock the operator paused looks like to `launchctl list`.
/// The marker is the only signal that tells the next pass apart from a paused
/// clock, so it retries activation instead of reporting the unit current.
const CLOCK_RELOAD_PENDING_FILE: &str = "clock.reload-pending";

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

    let deadline = Instant::now() + VERSION_PROBE_TIMEOUT;
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
                return Err(format!(
                    "--version timed out after {}s",
                    VERSION_PROBE_TIMEOUT.as_secs()
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait for --version failed: {error}"));
            }
        }
    }
}

/// Where the installed clock unit lives and what program it names.
///
/// Cheap by construction: reading the unit file answers both questions, and
/// callers on the per-minute tick path must not pay for a `--version` spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InstalledClockUnit {
    /// The launchd plist or systemd service that was found.
    pub(super) unit_path: PathBuf,
    /// Program path the unit names, when the unit file parses.
    pub(super) program: Option<PathBuf>,
    /// Whether the unit still invokes the compatibility alias `orbit sweep`.
    pub(super) legacy_invocation: bool,
}

/// How an installed unit's program disagreed with the running binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockUnitDrift {
    /// The unit file names no program path at all.
    ProgramUnreadable,
    /// The unit names a program that no longer exists — the launchd
    /// `EX_CONFIG` / penalty-box failure, where ticks stop without a signal.
    ProgramMissing {
        /// Path the unit named.
        previous: PathBuf,
    },
    /// The unit names a different binary that still exists.
    ProgramMoved {
        /// Path the unit named.
        previous: PathBuf,
    },
    /// The unit runs this binary through the compatibility alias
    /// `orbit sweep` instead of the canonical `orbit clock tick`.
    InvocationStale,
}

impl ClockUnitDrift {
    /// The program path the unit named, when it named one.
    pub fn previous_program(&self) -> Option<&Path> {
        match self {
            Self::ProgramUnreadable | Self::InvocationStale => None,
            Self::ProgramMissing { previous } | Self::ProgramMoved { previous } => {
                Some(previous.as_path())
            }
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::ProgramUnreadable => "named no program path".to_string(),
            Self::ProgramMissing { previous } => {
                format!("ran {}, which no longer exists", previous.display())
            }
            Self::ProgramMoved { previous } => format!("ran {}", previous.display()),
            Self::InvocationStale => "invoked the legacy `orbit sweep`".to_string(),
        }
    }
}

/// What a clock-unit convergence pass found, and what it did about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockUnitConvergence {
    /// No unit is installed on this host; there is nothing to converge.
    NoUnitInstalled,
    /// The installed unit already runs this binary.
    AlreadyCurrent {
        /// Unit file that was checked.
        unit_path: PathBuf,
        /// Program it names, which is this binary.
        program: PathBuf,
    },
    /// The unit named a stale program and was rewritten to this binary.
    Rewritten(ClockUnitRewrite),
    /// The unit already names this binary, but an earlier rewrite left it
    /// unregistered, so this pass retried activation without touching the file.
    Reloaded(ClockUnitReload),
}

/// The repair a convergence pass applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockUnitRewrite {
    /// Unit file that was rewritten.
    pub unit_path: PathBuf,
    /// Why the installed unit was stale.
    pub drift: ClockUnitDrift,
    /// Program the unit now names.
    pub program: PathBuf,
    /// Every unit file written by the repair.
    pub files_written: Vec<PathBuf>,
    /// Whether the unit manager re-registered the rewritten unit. A paused
    /// clock is rewritten but deliberately left inactive.
    pub reactivated: bool,
    /// Commands the operator must run when re-registration failed.
    pub manual_steps: Vec<String>,
}

/// The activation retry a convergence pass applied to an already-current unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockUnitReload {
    /// Unit file whose registration was retried.
    pub unit_path: PathBuf,
    /// Program it names, which is this binary.
    pub program: PathBuf,
    /// Whether the unit manager re-registered the unit this time.
    pub reactivated: bool,
    /// Commands the operator must run when re-registration failed again.
    pub manual_steps: Vec<String>,
}

impl ClockUnitConvergence {
    /// One operator-facing line describing the pass.
    pub fn summary(&self) -> String {
        match self {
            Self::NoUnitInstalled => {
                "no sweep clock unit is installed on this host; nothing to converge".to_string()
            }
            Self::AlreadyCurrent { unit_path, program } => format!(
                "clock unit {} already runs this binary ({})",
                unit_path.display(),
                program.display()
            ),
            Self::Rewritten(rewrite) => {
                let activation = if rewrite.reactivated {
                    "reloaded"
                } else if rewrite.manual_steps.is_empty() {
                    "left paused"
                } else {
                    "NOT reloaded"
                };
                format!(
                    "rewrote clock unit {}: it {} and now runs {} ({activation})",
                    rewrite.unit_path.display(),
                    rewrite.drift.describe(),
                    rewrite.program.display()
                )
            }
            Self::Reloaded(reload) => {
                let activation = if reload.reactivated {
                    "reloaded"
                } else {
                    "still NOT reloaded"
                };
                format!(
                    "clock unit {} already runs this binary ({}) but an earlier repair left it unregistered ({activation})",
                    reload.unit_path.display(),
                    reload.program.display()
                )
            }
        }
    }

    /// Commands the operator still has to run, if any.
    pub fn manual_steps(&self) -> &[String] {
        match self {
            Self::NoUnitInstalled | Self::AlreadyCurrent { .. } => &[],
            Self::Rewritten(rewrite) => &rewrite.manual_steps,
            Self::Reloaded(reload) => &reload.manual_steps,
        }
    }

    /// Whether the pass left the clock needing operator follow-up.
    pub fn needs_follow_up(&self) -> bool {
        !self.manual_steps().is_empty()
    }
}

/// Point the installed clock unit at the running binary when it has drifted.
///
/// A unit that names a moved or deleted binary keeps its schedule but fails
/// every wake-up, so this runs as part of `orbit update` convergence rather
/// than only when an operator reaches for `orbit clock repair`.
pub fn converge_clock_unit(global_root: &Path) -> Result<ClockUnitConvergence, OrbitError> {
    let running = RunningBinary::current()?;
    converge_clock_unit_with(
        global_root,
        &running.path,
        ClockPlatform::current(),
        &NativeClockCommandRunner,
        &orbit_common::fs::path::home_dir()?,
    )
}

/// [`converge_clock_unit`] against an explicit binary, platform, and home.
pub(super) fn converge_clock_unit_with(
    global_root: &Path,
    program: &Path,
    platform: ClockPlatform,
    runner: &dyn ClockCommandRunner,
    home: &Path,
) -> Result<ClockUnitConvergence, OrbitError> {
    let Some(installed) = installed_clock_unit_at(home, platform) else {
        return Ok(ClockUnitConvergence::NoUnitInstalled);
    };
    let reload_pending = clock_reload_pending_path(global_root).exists();
    let Some(drift) = clock_unit_drift(&installed, program) else {
        if !reload_pending {
            return Ok(ClockUnitConvergence::AlreadyCurrent {
                unit_path: installed.unit_path,
                program: program.to_path_buf(),
            });
        }
        // The file already names this binary, but the pass that wrote it could
        // not re-register it. Retrying only the activation is what makes
        // `orbit update` / `orbit clock repair` safe to re-run after that.
        let (reactivated, manual_steps) = activate_unit(platform, runner, home);
        if reactivated {
            clear_clock_reload_pending(global_root)?;
        }
        return Ok(ClockUnitConvergence::Reloaded(ClockUnitReload {
            unit_path: installed.unit_path,
            program: program.to_path_buf(),
            reactivated,
            manual_steps,
        }));
    };

    // Ask the manager whether the unit is registered *before* rewriting it: a
    // clock the operator paused must come back paused, not running. A unit an
    // earlier failed reload left unloaded is not paused, so a pending marker
    // counts as registered.
    let was_registered = reload_pending
        || runner
            .run(&manager_status_command(platform))
            .unwrap_or(false);
    let settings = load_clock_settings(global_root)?;
    let orbit_bin = program.to_string_lossy().to_string();
    let files_written = match platform {
        ClockPlatform::Launchd => {
            vec![write_launchd_unit(global_root, &orbit_bin, settings, home)?]
        }
        ClockPlatform::Systemd => write_systemd_units(&orbit_bin, settings, home)?,
    };
    let (reactivated, manual_steps) = if was_registered {
        activate_unit(platform, runner, home)
    } else {
        (false, Vec::new())
    };
    if was_registered && !reactivated {
        mark_clock_reload_pending(global_root, &installed.unit_path)?;
    } else if reload_pending {
        clear_clock_reload_pending(global_root)?;
    }

    Ok(ClockUnitConvergence::Rewritten(ClockUnitRewrite {
        unit_path: installed.unit_path,
        drift,
        program: program.to_path_buf(),
        files_written,
        reactivated,
        manual_steps,
    }))
}

/// Re-register the installed unit with its manager and name the commands the
/// operator has to run when the manager refuses.
fn activate_unit(
    platform: ClockPlatform,
    runner: &dyn ClockCommandRunner,
    home: &Path,
) -> (bool, Vec<String>) {
    match platform {
        ClockPlatform::Launchd => {
            let reactivated = reload_launchd_unit(runner, home);
            let manual_steps = launchd_manual_steps(reactivated, &launchd_plist_path(home));
            (reactivated, manual_steps)
        }
        ClockPlatform::Systemd => {
            let reactivated = restart_systemd_unit(runner);
            (reactivated, systemd_manual_steps(reactivated))
        }
    }
}

/// Where the reload-pending marker lives for this Orbit root.
pub(super) fn clock_reload_pending_path(global_root: &Path) -> PathBuf {
    global_root.join(CLOCK_RELOAD_PENDING_FILE)
}

fn mark_clock_reload_pending(global_root: &Path, unit_path: &Path) -> Result<(), OrbitError> {
    let path = clock_reload_pending_path(global_root);
    let content = format!(
        "# Orbit rewrote the sweep clock unit but the unit manager did not re-register it.\n\
         # `orbit clock repair` retries the reload while this file exists.\n{}\n",
        unit_path.display()
    );
    atomic_write_text(&path, &content)
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", path.display())))
}

/// Forget a pending reload: activation succeeded, or the operator set the
/// clock's state explicitly and a retry would second-guess them.
pub(super) fn clear_clock_reload_pending(global_root: &Path) -> Result<(), OrbitError> {
    let path = clock_reload_pending_path(global_root);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(OrbitError::Io(format!(
            "remove {}: {error}",
            path.display()
        ))),
    }
}

/// The installed unit and the program it names, without probing that program.
pub(super) fn installed_clock_unit_at(
    home: &Path,
    platform: ClockPlatform,
) -> Option<InstalledClockUnit> {
    match discover_clock_unit_program(home, platform)? {
        Ok((unit_path, program, legacy_invocation)) => Some(InstalledClockUnit {
            unit_path,
            program: Some(program),
            legacy_invocation,
        }),
        Err((unit_path, _reason)) => Some(InstalledClockUnit {
            unit_path,
            program: None,
            legacy_invocation: false,
        }),
    }
}

/// One warning line for a pass running from a binary the installed unit does
/// not name, or `None` when the two agree.
///
/// Diagnostic only: a host without a home directory, a readable unit, or a
/// resolvable executable gets no warning rather than a failed sweep.
pub fn clock_unit_drift_warning() -> Option<String> {
    let running = RunningBinary::current().ok()?;
    let home = orbit_common::fs::path::home_dir().ok()?;
    clock_unit_drift_warning_at(&home, ClockPlatform::current(), &running.path)
}

pub(super) fn clock_unit_drift_warning_at(
    home: &Path,
    platform: ClockPlatform,
    running: &Path,
) -> Option<String> {
    let installed = installed_clock_unit_at(home, platform)?;
    // A unit that runs this binary through the legacy alias is reported by
    // `orbit doctor` and `orbit clock status`; it is not a wrong-binary tick.
    let drift = match clock_unit_drift(&installed, running)? {
        ClockUnitDrift::InvocationStale => return None,
        drift => drift,
    };
    Some(format!(
        "warning: the installed clock unit {} {}, not this binary {}; scheduled ticks do not run this build. Run `orbit clock repair` to point the unit at it.",
        installed.unit_path.display(),
        drift.describe(),
        running.display()
    ))
}

/// Compare an installed unit to the running binary.
fn clock_unit_drift(installed: &InstalledClockUnit, running: &Path) -> Option<ClockUnitDrift> {
    let Some(program) = installed.program.as_deref() else {
        return Some(ClockUnitDrift::ProgramUnreadable);
    };
    if !program.exists() {
        return Some(ClockUnitDrift::ProgramMissing {
            previous: program.to_path_buf(),
        });
    }
    if !same_program(program, running) {
        return Some(ClockUnitDrift::ProgramMoved {
            previous: program.to_path_buf(),
        });
    }
    installed
        .legacy_invocation
        .then_some(ClockUnitDrift::InvocationStale)
}

type ClockUnitProgram = (PathBuf, PathBuf, bool);
type ClockUnitParseError = (PathBuf, String);

fn discover_clock_unit_program(
    home: &Path,
    platform: ClockPlatform,
) -> Option<Result<ClockUnitProgram, ClockUnitParseError>> {
    let unit_path = match platform {
        ClockPlatform::Launchd => launchd_plist_path(home),
        ClockPlatform::Systemd => systemd_service_path(home),
    };
    if !unit_path.exists() {
        return None;
    }
    let contents = match fs::read_to_string(&unit_path) {
        Ok(contents) => contents,
        Err(error) => {
            return Some(Err((
                unit_path,
                format!("could not read unit file: {error}"),
            )));
        }
    };
    let program = match platform {
        ClockPlatform::Launchd => parse_launchd_program(&contents),
        ClockPlatform::Systemd => parse_systemd_exec_start(&contents),
    };
    let legacy_invocation = match platform {
        ClockPlatform::Launchd => launchd_arguments(&contents)
            .is_some_and(|arguments| arguments.get(1).is_some_and(|arg| arg == "sweep")),
        ClockPlatform::Systemd => systemd_arguments(&contents)
            .is_some_and(|arguments| arguments.first().is_some_and(|arg| arg == "sweep")),
    };
    match program {
        Some(program) if !program.is_empty() => {
            Some(Ok((unit_path, PathBuf::from(program), legacy_invocation)))
        }
        _ => Some(Err((
            unit_path,
            "unit file does not name an orbit program path".to_string(),
        ))),
    }
}

fn parse_launchd_program(plist: &str) -> Option<String> {
    if let Some(program) = launchd_arguments(plist).and_then(|args| args.into_iter().next()) {
        return Some(program);
    }
    plist
        .split("<key>Program</key>")
        .nth(1)
        .and_then(first_plist_string)
}

fn launchd_arguments(plist: &str) -> Option<Vec<String>> {
    let args = plist.split("<key>ProgramArguments</key>").nth(1)?;
    let array = args.split("<array>").nth(1)?.split("</array>").next()?;
    let mut values = Vec::new();
    let mut remaining = array;
    while let Some(start) = remaining.find("<string>") {
        remaining = &remaining[start + "<string>".len()..];
        let end = remaining.find("</string>")?;
        values.push(remaining[..end].trim().to_string());
        remaining = &remaining[end + "</string>".len()..];
    }
    (!values.is_empty()).then_some(values)
}

fn first_plist_string(fragment: &str) -> Option<String> {
    let start = fragment.find("<string>")? + "<string>".len();
    let end = fragment[start..].find("</string>")?;
    let value = fragment[start..start + end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn parse_systemd_exec_start(unit: &str) -> Option<String> {
    for line in unit.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("ExecStart=") else {
            continue;
        };
        let rest = rest.trim();
        if let Some(stripped) = rest.strip_prefix('"') {
            return stripped.split('"').next().map(str::to_string);
        }
        let program = rest.split_whitespace().next()?.to_string();
        if !program.is_empty() {
            return Some(program);
        }
    }
    None
}

fn systemd_arguments(unit: &str) -> Option<Vec<String>> {
    let line = unit
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("ExecStart="))?
        .trim();
    let rest = if let Some(stripped) = line.strip_prefix('"') {
        let end = stripped.find('"')?;
        &stripped[end + 1..]
    } else {
        line.split_once(char::is_whitespace)
            .map(|(_, rest)| rest)
            .unwrap_or("")
    };
    Some(rest.split_whitespace().map(ToString::to_string).collect())
}

fn same_program(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn normalize_version(raw: &str) -> String {
    let line = raw
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    line.split_whitespace()
        .rev()
        .find(|token| token.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap_or(line.trim())
        .trim_start_matches('v')
        .to_string()
}

fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToString::to_string)
}

fn display_opt_path(path: &Option<PathBuf>) -> String {
    path.as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<unknown>".to_string())
}
