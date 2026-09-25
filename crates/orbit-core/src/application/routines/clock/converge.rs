//! Repair an installed clock unit that has drifted from the running binary.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;

use super::inspect::RunningBinary;
use super::install::{
    launchd_manual_steps, launchd_plist_path, reload_launchd_unit, restart_systemd_unit,
    systemd_manual_steps, write_launchd_unit, write_systemd_units,
};
use super::manager::{ClockCommandRunner, ClockPlatform, NativeClockCommandRunner};
use super::program::{discover_clock_unit_program, same_program};
use super::settings::load_clock_settings;
use super::status::manager_status_command;

/// Marker written next to `clock.toml` when a convergence pass rewrote a
/// registered unit but the manager would not re-register it.
///
/// `launchctl load` failing after the `unload` leaves the job unloaded, which
/// is exactly what a clock the operator paused looks like to `launchctl list`.
/// The marker is the only signal that tells the next pass apart from a paused
/// clock, so it retries activation instead of reporting the unit current.
const CLOCK_RELOAD_PENDING_FILE: &str = "clock.reload-pending";

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
