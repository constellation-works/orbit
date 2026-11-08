//! systemd timer schedulability and sweep service health.

use orbit_common::OrbitError;

use crate::inspect::systemd_recovery_issue;
use crate::manager::{ClockCommandRunner, ManagerCommand, ManagerCommandOutput};
use crate::settings::SYSTEMD_UNIT;

use super::{manager_error, query};

fn query_systemd_service_health(
    runner: &dyn ClockCommandRunner,
    now_usec: Option<u64>,
) -> Result<SystemdServiceHealth, OrbitError> {
    let command = ManagerCommand {
        program: "systemctl",
        args: vec![
            "--user".into(),
            "show".into(),
            format!("{SYSTEMD_UNIT}.service"),
            "--property=ActiveState".into(),
            "--property=ExecMainStartTimestampMonotonic".into(),
            "--property=Result".into(),
            "--property=TimeoutStartUSec".into(),
            "--property=KillMode".into(),
        ],
    };
    let output = runner
        .stdout(&command)?
        .ok_or_else(|| manager_error::manager_command_error(&command))?;
    Ok(systemd_service_health(&output, now_usec))
}

/// Apply the service health rules shared by status and activation verification.
/// Service health can grant schedulability while a loaded timer waits for its
/// in-flight oneshot to finish; it cannot revoke a finite next elapse.
pub(crate) fn systemd_schedulability_with_health(
    runner: &dyn ClockCommandRunner,
    details: Option<&SystemdClockDetails>,
    now_usec: Option<u64>,
) -> (bool, Option<String>) {
    let mut schedulable = details.is_some_and(SystemdClockDetails::is_schedulable);
    match query_systemd_service_health(runner, now_usec) {
        Ok(service) => {
            if service.running
                && service.issue.is_none()
                && details.is_some_and(|details| {
                    details.loaded == Some(true)
                        && details.is_running()
                        && !details.next_elapse_known
                })
            {
                // With OnUnitActiveSec, systemd waits for the oneshot to
                // finish before assigning the timer's next elapse.
                schedulable = true;
            }
            (schedulable, service.issue)
        }
        Err(error) => (
            schedulable,
            Some(format!(
                "systemd tick health could not be verified: {error}"
            )),
        ),
    }
}

struct SystemdServiceHealth {
    running: bool,
    issue: Option<String>,
}

/// Use the manager's monotonic start timestamp, so wall-clock adjustments
/// cannot hide an overdue oneshot (which stays `activating` while running).
fn systemd_service_health(output: &str, now_usec: Option<u64>) -> SystemdServiceHealth {
    let property = |name| {
        output.lines().find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key == name).then_some(value.trim())
        })
    };
    let mut issues = Vec::new();
    let running = matches!(property("ActiveState"), Some("activating" | "active"));
    if running
        && let Some(start) = property("ExecMainStartTimestampMonotonic")
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
        && let Some(elapsed) = now_usec.and_then(|now| now.checked_sub(start))
        && std::time::Duration::from_micros(elapsed) >= crate::TICK_DEADLINE
    {
        issues.push(format!(
            "tick has been running for {}s, exceeding its {}s deadline",
            elapsed / 1_000_000,
            crate::TICK_DEADLINE.as_secs()
        ));
    }
    if let Some(result) =
        property("Result").filter(|result| !result.is_empty() && *result != "success")
    {
        issues.push(format!("most recent tick failed ({result})"));
    }
    if let Some(issue) = systemd_recovery_issue(property("TimeoutStartUSec"), property("KillMode"))
    {
        issues.push(issue);
    }
    SystemdServiceHealth {
        running,
        issue: (!issues.is_empty()).then(|| format!("systemd sweep service: {}; recovery: inspect `systemctl --user status {SYSTEMD_UNIT}.service` and run `orbit clock repair`", issues.join("; "))),
    }
}

#[cfg(unix)]
pub(crate) fn monotonic_usec() -> Option<u64> {
    let mut value = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: value points to a valid writable timespec; clock_gettime only
    // fills it. CLOCK_MONOTONIC is the clock systemd uses for its timestamps.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) } != 0 {
        return None;
    }
    u64::try_from(value.tv_sec)
        .ok()?
        .checked_mul(1_000_000)?
        .checked_add(u64::try_from(value.tv_nsec).ok()? / 1_000)
}

#[cfg(not(unix))]
pub(crate) fn monotonic_usec() -> Option<u64> {
    None
}

/// A failed `is-enabled` probe is a recognized disabled or missing unit only
/// when the diagnostic names a unit state. A manager transport failure is
/// ruled out first because systemd reuses the same errno text for both: a
/// missing unit file reports `Failed to get unit file state: No such file or
/// directory`, while an unreachable user bus reports `Failed to connect to
/// bus: No such file or directory`. Only the former is a disabled clock.
pub(crate) fn systemd_reports_disabled_or_missing(output: &ManagerCommandOutput) -> bool {
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

pub(crate) fn systemd_reports_missing(output: &ManagerCommandOutput) -> bool {
    let diagnostic = format!("{}\n{}", output.stdout, output.stderr).to_ascii_lowercase();
    !systemd_reports_transport_failure(&diagnostic)
        && [
            "not-found",
            "not found",
            "failed to get unit file state: no such file or directory",
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

#[derive(Debug, Default)]
pub(crate) struct SystemdClockDetails {
    pub(super) loaded: Option<bool>,
    pub(super) running: Option<bool>,
    pub(super) last_tick_at: Option<String>,
    /// Wall-clock next elapse from `NextElapseUSecRealtime`. Monotonic values
    /// are durations from boot, not a next-tick timestamp.
    pub(super) next_tick_at: Option<String>,
    next_elapse_known: bool,
}

impl SystemdClockDetails {
    fn parse(output: &str) -> Self {
        let property = |name: &str| {
            output.lines().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                (key == name)
                    .then(|| manager_error::useful_manager_value(value))
                    .flatten()
            })
        };
        let next_tick_at = property("NextElapseUSecRealtime");
        let next_elapse_monotonic = property("NextElapseUSecMonotonic");
        Self {
            loaded: property("LoadState").map(|value| value == "loaded"),
            running: match property("ActiveState").as_deref() {
                Some("active") => Some(true),
                Some("inactive") => Some(false),
                _ => None,
            },
            last_tick_at: property("LastTriggerUSec"),
            next_elapse_known: next_tick_at.is_some() || next_elapse_monotonic.is_some(),
            next_tick_at,
        }
    }

    pub(crate) fn is_schedulable(&self) -> bool {
        self.loaded == Some(true) && self.running == Some(true) && self.next_elapse_known
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.running == Some(false) && !self.next_elapse_known
    }

    pub(crate) fn is_running(&self) -> bool {
        self.running == Some(true)
    }
}

pub(crate) fn query_systemd_clock_details(
    runner: &dyn ClockCommandRunner,
) -> Result<SystemdClockDetails, OrbitError> {
    let command = query::systemd_next_trigger_command();
    let output = runner.stdout(&command)?.ok_or_else(|| {
        OrbitError::Execution(format!(
            "{} failed; inspect `systemctl --user status {SYSTEMD_UNIT}.timer`",
            command.display()
        ))
    })?;
    Ok(SystemdClockDetails::parse(&output))
}
