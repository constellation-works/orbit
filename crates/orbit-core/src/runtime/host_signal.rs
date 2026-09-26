//! Host lifecycle signals that bear on admitting new work [ORB-12968].
//!
//! A host that has scheduled its own shutdown or reboot will kill every run
//! started before it, so unattended admission points (routine and auto-task
//! fires from the scheduler tick, drain waves, the registry ship sweep) hold
//! new work while one is pending. Runs already in flight are never touched:
//! the probe only answers "is a shutdown scheduled", and callers decide what
//! not to *start*.
//!
//! On systemd hosts logind persists the pending schedule, unprivileged, in
//! [`SYSTEMD_SCHEDULED_SHUTDOWN_PATH`] — the same state it exposes as the
//! `org.freedesktop.login1.Manager.ScheduledShutdown` D-Bus property. The file
//! is removed by `shutdown -c`, and `/run` is a tmpfs, so a reboot clears it
//! too: admission resumes on its own either way. Other platforms have no probe
//! and report no schedule.

use std::io;
use std::sync::Arc;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

/// Where systemd-logind records a pending `shutdown` schedule.
pub const SYSTEMD_SCHEDULED_SHUTDOWN_PATH: &str = "/run/systemd/shutdown/scheduled";

/// Stable code every hold reports, so schedulers and readiness can branch on it.
pub const HOST_SHUTDOWN_SCHEDULED: &str = "host_shutdown_scheduled";

/// A pending host shutdown or reboot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScheduledShutdown {
    /// What the host will do: `reboot`, `poweroff`, `halt`, `kexec`, ….
    pub mode: String,
    /// When it will do it.
    pub scheduled_at: DateTime<Utc>,
    /// Where the schedule was read from, for diagnostics.
    pub source: String,
}

impl ScheduledShutdown {
    /// One-line description naming the mode and time, for logs and refusals.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "host {} scheduled for {}",
            self.mode,
            self.scheduled_at.to_rfc3339_opts(SecondsFormat::Secs, true)
        )
    }

    /// The reason every held admission reports.
    #[must_use]
    pub fn hold_reason(&self) -> String {
        format!(
            "{HOST_SHUTDOWN_SCHEDULED}: {}; new runs are held until the schedule is cancelled \
             or the host restarts",
            self.describe()
        )
    }
}

/// Answers whether the host has a shutdown or reboot pending.
pub trait HostSignalProbe: Send + Sync {
    /// The pending schedule, or `None` when there is none — or when the
    /// platform offers no way to tell.
    fn scheduled_shutdown(&self) -> Option<ScheduledShutdown>;
}

/// The probe for this platform: logind's schedule file on Linux, nothing
/// elsewhere. This crate's own unit tests never read the real host schedule —
/// a reboot pending on the machine running them must not change their
/// outcome — so they exercise the hold through injected probes instead.
#[must_use]
pub fn default_host_signal_probe() -> Arc<dyn HostSignalProbe> {
    if cfg!(all(target_os = "linux", not(test))) {
        Arc::new(SystemdScheduledShutdownProbe)
    } else {
        Arc::new(FixedHostSignals::none())
    }
}

/// Reads a logind `scheduled` file. A missing file is "nothing scheduled";
/// an unreadable or malformed one is logged and treated the same way, because
/// the hold is a courtesy to the next boot and must not wedge admission on a
/// file Orbit does not own.
#[derive(Debug, Clone, Default)]
pub struct SystemdScheduledShutdownProbe;

impl HostSignalProbe for SystemdScheduledShutdownProbe {
    fn scheduled_shutdown(&self) -> Option<ScheduledShutdown> {
        scheduled_shutdown_from_read(std::fs::read_to_string(SYSTEMD_SCHEDULED_SHUTDOWN_PATH))
    }
}

/// Interpret the fixed logind path's read result. Keeping this separate lets
/// tests exercise file errors without granting the production probe an
/// arbitrary path.
pub(crate) fn scheduled_shutdown_from_read(
    contents: io::Result<String>,
) -> Option<ScheduledShutdown> {
    let contents = match contents {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(
                target: "orbit.core.host_signal",
                path = SYSTEMD_SCHEDULED_SHUTDOWN_PATH,
                error = %error,
                "unreadable scheduled-shutdown file; admission is not held",
            );
            return None;
        }
    };
    let parsed = parse_systemd_schedule(&contents, SYSTEMD_SCHEDULED_SHUTDOWN_PATH);
    if parsed.is_none() && !is_dry_run(&contents) {
        tracing::warn!(
            target: "orbit.core.host_signal",
            path = SYSTEMD_SCHEDULED_SHUTDOWN_PATH,
            "malformed scheduled-shutdown file; admission is not held",
        );
    }
    parsed
}

/// A probe with a fixed answer: no host signals, or an injected schedule.
#[derive(Debug, Clone, Default)]
pub struct FixedHostSignals {
    shutdown: Option<ScheduledShutdown>,
}

impl FixedHostSignals {
    /// Reports no pending shutdown.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Reports `shutdown` as pending.
    #[must_use]
    pub fn scheduled(shutdown: ScheduledShutdown) -> Self {
        Self {
            shutdown: Some(shutdown),
        }
    }
}

impl HostSignalProbe for FixedHostSignals {
    fn scheduled_shutdown(&self) -> Option<ScheduledShutdown> {
        self.shutdown.clone()
    }
}

/// Parse logind's `KEY=value` schedule file. `USEC` is the wall-clock
/// deadline in microseconds since the epoch; `MODE` is the action. A
/// `dry-*` mode (`shutdown -k`) only sends wall messages and is not a
/// shutdown, so it holds nothing.
pub(crate) fn parse_systemd_schedule(contents: &str, source: &str) -> Option<ScheduledShutdown> {
    let mut usec = None;
    let mut mode = None;
    for line in contents.lines() {
        match line.trim().split_once('=') {
            Some(("USEC", value)) => usec = value.trim().parse::<i64>().ok(),
            Some(("MODE", value)) => mode = Some(value.trim().to_string()),
            _ => {}
        }
    }
    let mode = mode.filter(|mode| !mode.is_empty() && !mode.starts_with("dry-"))?;
    let scheduled_at = DateTime::<Utc>::from_timestamp_micros(usec.filter(|usec| *usec > 0)?)?;
    Some(ScheduledShutdown {
        mode,
        scheduled_at,
        source: source.to_string(),
    })
}

fn is_dry_run(contents: &str) -> bool {
    contents
        .lines()
        .any(|line| line.trim().starts_with("MODE=dry-"))
}
