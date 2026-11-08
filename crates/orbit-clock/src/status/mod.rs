//! Query the native manager for the clock's loaded, enabled and health state.

mod doctor;
mod launchd;
mod manager_error;
mod query;
mod systemd;

pub use doctor::doctor_clock_not_running_issue;
pub use query::clock_status;

pub(super) use launchd::{LaunchdHealthProbe, launchd_health_issue, launchd_reports_not_loaded};
pub(super) use manager_error::{
    clock_manager_probe_error, clock_manager_unavailable_error, manager_command_error,
    manager_probe_failure, systemd_unschedulable_error,
};
pub(super) use query::clock_status_with;
pub(super) use query::{clock_status_from, launchd_manager_probe_command, manager_status_command};
pub(super) use systemd::{
    SystemdClockDetails, monotonic_usec, query_systemd_clock_details,
    systemd_reports_disabled_or_missing, systemd_reports_missing,
    systemd_schedulability_with_health,
};

/// Host clock settings and the scheduling state observed from its native manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockStatus {
    /// Sweep interval persisted in the host's clock settings, in seconds.
    pub configured_cadence_seconds: u64,
    /// Configured interval in seconds, present only when the clock is schedulable.
    pub effective_cadence_seconds: Option<u64>,
    /// Whether the native manager reports the clock as enabled or loaded.
    pub enabled: bool,
    /// Whether the native manager reports the unit as loaded.
    pub loaded: bool,
    /// Whether the native timer is currently active/waiting, when exposed.
    pub running: Option<bool>,
    /// Whether the native clock is active with a future trigger. A stopped
    /// clock is intentionally not schedulable and is not unhealthy for
    /// `orbit clock status`. [`doctor_clock_not_running_issue`] is the
    /// separate signal `orbit doctor` uses, because a paused clock and a
    /// timer that was never enabled look the same to the unit manager.
    pub schedulable: bool,
    /// Actionable detail when the manager state is inconsistent or an enabled
    /// clock cannot be shown to have a future trigger.
    pub health_issue: Option<String>,
    /// Most recent native timer trigger, in the manager's display format.
    pub last_tick_at: Option<String>,
    /// Next native timer trigger, in the manager's display format.
    pub next_tick_at: Option<String>,
    /// Native manager name: `launchd` or `systemd`.
    pub platform: &'static str,
}

/// The one health classification of a [`ClockStatus`]. Each surface renders
/// it in its own vocabulary, so the variants keep apart every state a surface
/// labels differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockHealth {
    /// Enabled with a future trigger and no reported issue.
    Healthy,
    /// Enabled with a future trigger, but the manager reported an issue.
    ScheduledWithIssue,
    /// Enabled without a verified future trigger.
    Missed,
    /// Paused and stopped.
    Paused,
    /// Paused and not running, but the manager reported an issue.
    PausedWithIssue,
    /// Paused, yet the native timer is still running.
    RunningWhilePaused,
}

impl ClockStatus {
    /// Classify this status; see [`ClockHealth`].
    pub fn health(&self) -> ClockHealth {
        match (self.enabled, self.schedulable, self.health_issue.is_some()) {
            (false, _, _) if self.running == Some(true) => ClockHealth::RunningWhilePaused,
            (false, _, true) => ClockHealth::PausedWithIssue,
            (false, _, false) => ClockHealth::Paused,
            (true, false, _) => ClockHealth::Missed,
            (true, true, true) => ClockHealth::ScheduledWithIssue,
            (true, true, false) => ClockHealth::Healthy,
        }
    }
}
