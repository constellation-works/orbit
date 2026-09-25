//! Query the native manager for the clock's loaded, enabled and health state.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::path::home_dir;

use super::inspect::{
    ClockUnitVerdict, RunningBinary, inspect_clock_unit_at, probe_program_version,
};
use super::manager::{
    ClockCommandRunner, ClockPlatform, ManagerCommand, ManagerCommandOutput,
    NativeClockCommandRunner,
};
use super::settings::{ClockSettings, LAUNCHD_LABEL, SYSTEMD_UNIT, load_clock_settings};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockStatus {
    pub configured_cadence_seconds: u64,
    pub effective_cadence_seconds: Option<u64>,
    pub enabled: bool,
    /// Whether the native manager reports the unit as loaded.
    pub loaded: bool,
    /// Whether the native timer is currently active/waiting, when exposed.
    pub running: Option<bool>,
    /// Whether an enabled native clock has a future trigger. A paused clock is
    /// intentionally not schedulable and is not unhealthy.
    pub schedulable: bool,
    /// Actionable detail when an enabled clock cannot be shown to have a
    /// future trigger.
    pub health_issue: Option<String>,
    /// Most recent native timer trigger, in the manager's display format.
    pub last_tick_at: Option<String>,
    /// Next native timer trigger, in the manager's display format.
    pub next_tick_at: Option<String>,
    pub platform: &'static str,
}

pub(super) fn manager_status_command(platform: ClockPlatform) -> ManagerCommand {
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

pub(super) fn launchd_manager_probe_command() -> ManagerCommand {
    ManagerCommand {
        program: "launchctl",
        args: vec!["list".into()],
    }
}

/// `launchctl list <label>` only says the agent is loaded. The per-service
/// dump is what reveals a loaded agent that can no longer run.
fn launchd_print_command(uid: u32) -> ManagerCommand {
    ManagerCommand {
        program: "launchctl",
        args: vec!["print".into(), format!("gui/{uid}/{LAUNCHD_LABEL}")],
    }
}

fn systemd_next_trigger_command() -> ManagerCommand {
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

pub fn clock_status(global_root: &Path) -> Result<ClockStatus, OrbitError> {
    let platform = ClockPlatform::current();
    // Resolved only on macOS: a systemd host has no launchd agent to inspect,
    // and must not start failing `clock status` because HOME is unset.
    let launchd = match platform {
        ClockPlatform::Launchd => Some(LaunchdHealthProbe::current()?),
        ClockPlatform::Systemd => None,
    };
    clock_status_with(
        global_root,
        platform,
        &NativeClockCommandRunner,
        launchd.as_ref(),
    )
}

pub(super) fn clock_status_with(
    global_root: &Path,
    platform: ClockPlatform,
    runner: &dyn ClockCommandRunner,
    launchd: Option<&LaunchdHealthProbe>,
) -> Result<ClockStatus, OrbitError> {
    let settings = load_clock_settings(global_root)?;
    let status_command = manager_status_command(platform);
    let status_output = runner
        .probe(&status_command)
        .map_err(|error| clock_manager_probe_error(platform, &status_command, &error))?;
    // A `launchctl print` dump captured while deciding `enabled`, so the
    // health check below does not ask launchd twice.
    let mut launchd_dump = None;
    let enabled = match platform {
        ClockPlatform::Launchd if !status_output.success => {
            if launchd_reports_not_loaded(&status_output) {
                false
            } else {
                let (loaded, dump) =
                    launchd_loaded_without_list(runner, launchd, &status_command, &status_output)?;
                launchd_dump = dump;
                loaded
            }
        }
        _ => status_output.success,
    };
    let mut manager_details = None;
    let (schedulable, health_issue) = if platform == ClockPlatform::Systemd {
        match query_systemd_clock_details(runner) {
            Ok(details) => {
                let schedulable = enabled && details.is_schedulable();
                manager_details = Some(details);
                if !enabled {
                    (false, None)
                } else if schedulable {
                    (true, None)
                } else {
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
            Err(_) if systemd_reports_disabled_or_missing(&status_output) => (false, None),
            Err(error) => {
                return Err(clock_manager_unavailable_error(
                    platform,
                    &[(&status_command, &status_output)],
                    Some(&error),
                ));
            }
        }
    } else if enabled {
        match launchd.and_then(|probe| launchd_health_issue(probe, runner, launchd_dump)) {
            Some(issue) => (false, Some(issue)),
            None => (true, None),
        }
    } else {
        (false, None)
    };
    Ok(clock_status_from(
        settings,
        enabled,
        schedulable,
        platform,
        health_issue,
        manager_details,
    ))
}

/// A failed `is-enabled` probe is a recognized disabled or missing unit only
/// when the diagnostic names a unit state. A manager transport failure is
/// ruled out first because systemd reuses the same errno text for both: a
/// missing unit file reports `Failed to get unit file state: No such file or
/// directory`, while an unreachable user bus reports `Failed to connect to
/// bus: No such file or directory`. Only the former is a disabled clock.
pub(super) fn systemd_reports_disabled_or_missing(output: &ManagerCommandOutput) -> bool {
    let diagnostic = format!("{}\n{}", output.stdout, output.stderr).to_ascii_lowercase();
    if systemd_reports_transport_failure(&diagnostic) {
        return false;
    }

    [
        "disabled",
        "masked",
        "static",
        "indirect",
        "generated",
        "transient",
        "not-found",
        "not found",
        "failed to get unit file state",
        "could not be found",
    ]
    .iter()
    .any(|marker| diagnostic.contains(marker))
}

/// systemctl prefixes every bus-connection failure with `Failed to connect to`
/// (`... bus: No medium found`, `... bus: No such file or directory`,
/// `... user scope bus via local transport: ...`), regardless of the errno
/// that follows.
fn systemd_reports_transport_failure(lowercase_diagnostic: &str) -> bool {
    lowercase_diagnostic.contains("failed to connect to")
}

pub(super) fn launchd_reports_not_loaded(output: &ManagerCommandOutput) -> bool {
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
fn launchd_loaded_without_list(
    runner: &dyn ClockCommandRunner,
    launchd: Option<&LaunchdHealthProbe>,
    status_command: &ManagerCommand,
    status_output: &ManagerCommandOutput,
) -> Result<(bool, Option<String>), OrbitError> {
    let print_attempt = launchd.map(|probe| launchd_print_command(probe.uid));
    let print_output =
        match &print_attempt {
            Some(command) => Some(runner.probe(command).map_err(|error| {
                clock_manager_probe_error(ClockPlatform::Launchd, command, &error)
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

    let manager_command = launchd_manager_probe_command();
    let manager_output = runner.probe(&manager_command).map_err(|error| {
        clock_manager_probe_error(ClockPlatform::Launchd, &manager_command, &error)
    })?;
    if manager_output.success {
        return Ok((false, None));
    }
    let mut attempts = vec![(status_command, status_output)];
    if let (Some(command), Some(output)) = (&print_attempt, &print_output) {
        attempts.push((command, output));
    }
    attempts.push((&manager_command, &manager_output));
    Err(clock_manager_unavailable_error(
        ClockPlatform::Launchd,
        &attempts,
        None,
    ))
}

pub(super) fn clock_manager_unavailable_error<'a>(
    platform: ClockPlatform,
    attempts: &[(&'a ManagerCommand, &'a ManagerCommandOutput)],
    detail_error: Option<&OrbitError>,
) -> OrbitError {
    let mut diagnostics = attempts
        .iter()
        .map(|(command, output)| manager_probe_diagnostic(command, output))
        .collect::<Vec<_>>();
    if let Some(error) = detail_error {
        diagnostics.push(bounded_manager_text(&error.to_string()));
    }
    OrbitError::Execution(format!(
        "{} clock manager is unavailable; {}. Check that the per-user {} manager is running and that this process can query it",
        platform.name(),
        diagnostics.join("; "),
        platform.name(),
    ))
}

pub(super) fn manager_probe_failure(
    command: &ManagerCommand,
    output: &ManagerCommandOutput,
) -> OrbitError {
    OrbitError::Execution(manager_probe_diagnostic(command, output))
}

pub(super) fn clock_manager_probe_error(
    platform: ClockPlatform,
    command: &ManagerCommand,
    error: &OrbitError,
) -> OrbitError {
    OrbitError::Execution(format!(
        "{} clock manager is unavailable; `{}` could not run: {}. Check that the per-user {} manager is installed and running and that this process can query it",
        platform.name(),
        command.display(),
        bounded_manager_text(&error.to_string()),
        platform.name(),
    ))
}

fn manager_probe_diagnostic(command: &ManagerCommand, output: &ManagerCommandOutput) -> String {
    let exit = output.exit_code.map_or_else(
        || "by signal".to_string(),
        |code| format!("with exit code {code}"),
    );
    let stdout = bounded_manager_text(&output.stdout);
    let stderr = bounded_manager_text(&output.stderr);
    let mut detail = Vec::new();
    if !stdout.is_empty() {
        detail.push(format!("stdout: {stdout}"));
    }
    if !stderr.is_empty() {
        detail.push(format!("stderr: {stderr}"));
    }
    if detail.is_empty() {
        format!("`{}` failed {exit} without output", command.display())
    } else {
        format!(
            "`{}` failed {exit} ({})",
            command.display(),
            detail.join(", ")
        )
    }
}

fn bounded_manager_text(value: &str) -> String {
    const MAX_DIAGNOSTIC_CHARS: usize = 512;

    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let bounded = chars
        .by_ref()
        .take(MAX_DIAGNOSTIC_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

/// Host facts the launchd health checks need, injected so tests can drive a
/// missing program or a penalty-boxed agent without a real macOS host.
#[derive(Debug, Clone)]
pub(super) struct LaunchdHealthProbe {
    /// Home directory holding `Library/LaunchAgents`.
    pub(super) home: PathBuf,
    /// launchd GUI domain owner, as in `gui/<uid>/<label>`.
    pub(super) uid: u32,
    /// Binary the unit is compared against.
    pub(super) running: RunningBinary,
    /// How to ask the unit's program for its version. Production passes
    /// [`probe_program_version`], the same probe `orbit doctor` uses.
    pub(super) version_probe: fn(&Path) -> Result<String, String>,
}

impl LaunchdHealthProbe {
    fn current() -> Result<Self, OrbitError> {
        Ok(Self {
            home: home_dir()?,
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
fn launchd_health_issue(
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
    let command = launchd_print_command(uid);
    match runner.probe(&command) {
        Ok(output) if output.success => Ok(output.stdout),
        Ok(output) => Err(manager_probe_diagnostic(&command, &output)),
        Err(error) => Err(format!(
            "`{}` could not run: {}",
            command.display(),
            bounded_manager_text(&error.to_string())
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

#[derive(Debug, Default)]
pub(super) struct SystemdClockDetails {
    loaded: Option<bool>,
    running: Option<bool>,
    last_tick_at: Option<String>,
    /// Wall-clock next elapse from `NextElapseUSecRealtime`. Monotonic values
    /// are durations from boot, not a next-tick timestamp.
    next_tick_at: Option<String>,
    next_elapse_known: bool,
}

impl SystemdClockDetails {
    fn parse(output: &str) -> Self {
        let property = |name: &str| {
            output.lines().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                (key == name).then(|| useful_manager_value(value)).flatten()
            })
        };
        let next_tick_at = property("NextElapseUSecRealtime");
        let next_elapse_monotonic = property("NextElapseUSecMonotonic");
        Self {
            loaded: property("LoadState").map(|value| value == "loaded"),
            running: property("ActiveState").map(|value| value == "active"),
            last_tick_at: property("LastTriggerUSec"),
            next_elapse_known: next_tick_at.is_some() || next_elapse_monotonic.is_some(),
            next_tick_at,
        }
    }

    pub(super) fn is_schedulable(&self) -> bool {
        self.loaded == Some(true) && self.running == Some(true) && self.next_elapse_known
    }
}

pub(super) fn query_systemd_clock_details(
    runner: &dyn ClockCommandRunner,
) -> Result<SystemdClockDetails, OrbitError> {
    let command = systemd_next_trigger_command();
    let output = runner.stdout(&command)?.ok_or_else(|| {
        OrbitError::Execution(format!(
            "{} failed; inspect `systemctl --user status {SYSTEMD_UNIT}.timer`",
            command.display()
        ))
    })?;
    Ok(SystemdClockDetails::parse(&output))
}

pub(super) fn manager_command_error(command: &ManagerCommand) -> OrbitError {
    OrbitError::Execution(format!(
        "{} failed; recovery: {}",
        command.display(),
        command.display()
    ))
}

pub(super) fn systemd_unschedulable_error(action: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "systemd timer {action}, but the manager did not report it active with a finite future trigger; inspect `systemctl --user status {SYSTEMD_UNIT}.timer` and `journalctl --user -u {SYSTEMD_UNIT}.timer -u {SYSTEMD_UNIT}.service`"
    ))
}

fn useful_manager_value(raw: &str) -> Option<String> {
    let value = raw.trim();
    (!value.is_empty()
        && !matches!(
            value.to_ascii_lowercase().as_str(),
            "n/a" | "infinity" | "0"
        ))
    .then(|| value.to_string())
}

pub(super) fn clock_status_from(
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
        effective_cadence_seconds: (enabled && schedulable).then_some(settings.cadence_seconds),
        enabled,
        loaded: details.loaded.unwrap_or(enabled),
        running: details.running,
        schedulable,
        health_issue,
        last_tick_at: details.last_tick_at,
        next_tick_at: (enabled && schedulable)
            .then_some(details.next_tick_at)
            .flatten(),
        platform: platform.name(),
    }
}
