//! launchd loading, program inspection and sweep health.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::path::home_dir;

use crate::inspect::{
    ClockUnitVerdict, RunningBinary, inspect_clock_unit_at, probe_program_version,
};
use crate::manager::{ClockCommandRunner, ClockPlatform, ManagerCommand, ManagerCommandOutput};
use crate::settings::LAUNCHD_LABEL;

use super::{manager_error, query};

pub(crate) fn launchd_reports_not_loaded(output: &ManagerCommandOutput) -> bool {
    let diagnostic = format!("{}\n{}", output.stdout, output.stderr).to_ascii_lowercase();
    [
        "could not find service",
        "service not found",
        "no such process",
    ]
    .iter()
    .any(|marker| diagnostic.contains(marker))
}

/// Decide whether the launchd agent is loaded after `launchctl list <label>`
/// failed without naming a not-loaded state, and return the `launchctl print`
/// dump when that is what answered.
///
/// A macOS agent sandbox denies `launchctl list` outright (exit 1, no output)
/// while still allowing `launchctl print gui/<uid>/<label>` [DANI-10519], so
/// the per-service dump is consulted first: success means loaded, a
/// not-loaded diagnostic means paused. Only when `print` cannot answer either
/// way does the bare `launchctl list` decide between a paused clock and a
/// manager this process cannot query.
pub(super) fn launchd_loaded_without_list(
    runner: &dyn ClockCommandRunner,
    launchd: Option<&LaunchdHealthProbe>,
    status_command: &ManagerCommand,
    status_output: &ManagerCommandOutput,
) -> Result<(bool, Option<String>), OrbitError> {
    let print_attempt = launchd.map(|probe| query::launchd_print_command(probe.uid));
    let print_output = match &print_attempt {
        Some(command) => Some(runner.probe(command).map_err(|error| {
            manager_error::clock_manager_probe_error(ClockPlatform::Launchd, command, &error)
        })?),
        None => None,
    };
    if let Some(output) = &print_output {
        if output.success {
            return Ok((true, Some(output.stdout.clone())));
        }
        if launchd_reports_not_loaded(output) {
            return Ok((false, None));
        }
    }

    let manager_command = query::launchd_manager_probe_command();
    let manager_output = runner.probe(&manager_command).map_err(|error| {
        manager_error::clock_manager_probe_error(ClockPlatform::Launchd, &manager_command, &error)
    })?;
    if manager_output.success {
        return Ok((false, None));
    }
    let mut attempts = vec![(status_command, status_output)];
    if let (Some(command), Some(output)) = (&print_attempt, &print_output) {
        attempts.push((command, output));
    }
    attempts.push((&manager_command, &manager_output));
    Err(manager_error::clock_manager_unavailable_error(
        ClockPlatform::Launchd,
        &attempts,
        None,
    ))
}

/// Host facts the launchd health checks need, injected so tests can drive a
/// missing program or a penalty-boxed agent without a real macOS host.
#[derive(Debug, Clone)]
pub(crate) struct LaunchdHealthProbe {
    /// Home directory holding `Library/LaunchAgents`.
    pub(crate) home: PathBuf,
    /// launchd GUI domain owner, as in `gui/<uid>/<label>`.
    pub(crate) uid: u32,
    /// Binary the unit is compared against.
    pub(crate) running: RunningBinary,
    /// How to ask the unit's program for its version. Production passes
    /// [`probe_program_version`], the same probe `orbit doctor` uses.
    pub(crate) version_probe: fn(&Path) -> Result<String, String>,
}

impl LaunchdHealthProbe {
    pub(super) fn current() -> Result<Self, OrbitError> {
        Self::at(&home_dir()?)
    }

    pub(crate) fn at(home: &Path) -> Result<Self, OrbitError> {
        Ok(Self {
            home: home.to_path_buf(),
            uid: current_uid(),
            running: RunningBinary::current()?,
            version_probe: probe_program_version,
        })
    }
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid takes no arguments, touches no caller memory, and is
    // documented as always succeeding.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

/// Why an enabled launchd agent still cannot sweep, or `None` when nothing
/// contradicts a healthy clock.
///
/// launchd keeps reporting a loaded agent as loaded after its program starts
/// failing, so `launchctl list` alone always looked healthy [DANI-10386]. Two
/// independent signals are consulted:
///
/// 1. The unit's own program, through the inspection `orbit doctor
///    clock-unit` renders, so both surfaces agree on a program-path failure.
///    Only [`ClockUnitVerdict::Unrunnable`] is a health issue: a version or
///    path mismatch names a binary that still runs, so the clock still ticks.
/// 2. `launchctl print`, for the outcome of the runs that already happened.
///    `dump` is that output when the enabled decision already captured it.
pub(crate) fn launchd_health_issue(
    probe: &LaunchdHealthProbe,
    runner: &dyn ClockCommandRunner,
    dump: Option<String>,
) -> Option<String> {
    let inspection = inspect_clock_unit_at(
        &probe.home,
        ClockPlatform::Launchd,
        &probe.running,
        probe.version_probe,
    );
    if let ClockUnitVerdict::Unrunnable { reason } = &inspection.verdict {
        let program = inspection.program_path.as_ref().map_or_else(
            || LAUNCHD_LABEL.to_string(),
            |path| path.display().to_string(),
        );
        return Some(launchd_recovery(&format!(
            "launchd agent {LAUNCHD_LABEL} is loaded but its program cannot run ({program}: {reason}), so no sweep will fire"
        )));
    }

    let output = match dump.map_or_else(|| launchd_print_dump(probe.uid, runner), Ok) {
        Ok(dump) => dump,
        Err(diagnostic) => {
            return Some(launchd_recovery(&format!(
                "launchd agent {LAUNCHD_LABEL} is loaded but its state could not be verified ({diagnostic})"
            )));
        }
    };
    LaunchdClockDetails::parse(&output)
        .failure_summary()
        .map(|summary| launchd_recovery(&summary))
}

/// Run `launchctl print gui/<uid>/<label>` and return its dump, or the
/// diagnostic explaining why the agent's state could not be read.
fn launchd_print_dump(uid: u32, runner: &dyn ClockCommandRunner) -> Result<String, String> {
    let command = query::launchd_print_command(uid);
    match runner.probe(&command) {
        Ok(output) if output.success => Ok(output.stdout),
        Ok(output) => Err(manager_error::manager_probe_diagnostic(&command, &output)),
        Err(error) => Err(format!(
            "`{}` could not run: {}",
            command.display(),
            manager_error::bounded_manager_text(&error.to_string())
        )),
    }
}

fn launchd_recovery(issue: &str) -> String {
    format!(
        "{issue}; recovery: `orbit clock enable` rewrites the unit to this binary and reloads it"
    )
}

/// The `launchctl print` fields that expose a stalled agent.
#[derive(Debug, Default, PartialEq, Eq)]
struct LaunchdClockDetails {
    /// Exit status of the most recent run, when one has exited.
    last_exit_code: Option<i32>,
    /// launchd is throttling the agent after repeated launch failures.
    penalty_box: bool,
}

impl LaunchdClockDetails {
    /// Read the top-level `last exit code` and `properties` lines. Nested
    /// blocks repeat some key names, so only the first match of each is used,
    /// and `properties` is matched on the whole trimmed key so the unrelated
    /// `jetsamproperties category` line cannot stand in for it.
    fn parse(output: &str) -> Self {
        let field = |name: &str| {
            output.lines().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                (key.trim() == name).then(|| value.trim())
            })
        };
        Self {
            last_exit_code: field("last exit code").and_then(launchd_exit_code),
            penalty_box: field("properties").is_some_and(|properties| {
                properties
                    .split('|')
                    .any(|property| property.trim() == "penalty box")
            }),
        }
    }

    /// One sentence naming every reason this agent is not sweeping.
    fn failure_summary(&self) -> Option<String> {
        let mut reasons = Vec::new();
        if let Some(code) = self.last_exit_code.filter(|code| *code != 0) {
            reasons.push(format!(
                "its most recent run exited {code} without sweeping"
            ));
        }
        if self.penalty_box {
            reasons.push(
                "launchd is holding it in the penalty box after repeated launch failures"
                    .to_string(),
            );
        }
        (!reasons.is_empty()).then(|| {
            format!(
                "launchd agent {LAUNCHD_LABEL} is loaded but {}",
                reasons.join(", and ")
            )
        })
    }
}

/// `last exit code = 78: EX_CONFIG` and `last exit code = 0` both appear;
/// `(never exited)` means the agent has not run yet and is not a failure.
fn launchd_exit_code(value: &str) -> Option<i32> {
    value
        .split(':')
        .next()
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .and_then(|code| code.parse::<i32>().ok())
}
