//! Weekly usage pressure, computed from this host's provider-limit
//! observations. Surfaces show it raw; under
//! `workflow.crew_selection = "usage_pressure"` task crew draws weigh it
//! [ORB-17770].

use chrono::{DateTime, Utc};
use orbit_types::identity::Crew;
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use serde::{Deserialize, Serialize};

use super::gate::{reading_covers, reading_lifetime};
use super::view::short_time;
use crate::OrbitRuntime;

const WEEK_MINUTES: u32 = 10080;

/// Raw headroom per fraction of a weekly window remaining. Unknown is neutral.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsagePressure {
    /// Raw, uncapped multiplier. Crew draws cap it under
    /// `workflow.crew_selection = "usage_pressure"`.
    pub pressure: f64,
    /// The weekly window that set the minimum, or `None` for neutral unknown usage.
    pub pressure_window: Option<PressureWindow>,
}

/// Evidence for the weekly window that sets a pressure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PressureWindow {
    /// Provider label, when reported. Duration, not this label, selects weekly windows.
    pub window: Option<String>,
    /// Model family covered, or `None` for the whole account.
    pub scope: Option<String>,
    /// Used share reported by the provider, in percent.
    pub used_percent: f64,
    /// Provider reset time.
    pub resets_at: DateTime<Utc>,
}

impl Default for UsagePressure {
    fn default() -> Self {
        Self {
            pressure: 1.0,
            pressure_window: None,
        }
    }
}

impl UsagePressure {
    /// The minimum valid weekly pressure among the supplied covering observations.
    pub(super) fn from_readings<'a>(
        observations: impl Iterator<Item = &'a ProviderLimitObservation>,
        now: DateTime<Utc>,
    ) -> Self {
        observations
            .filter_map(|reading| {
                if !reading.gating
                    || reading.window_minutes != Some(WEEK_MINUTES)
                    || matches!(
                        reading.source,
                        ProviderLimitSource::Error | ProviderLimitSource::Ledger
                    )
                    || reading_lifetime(reading) <= now
                {
                    return None;
                }
                let resets_at = reading.resets_at?;
                let used_percent = reading
                    .used_percent
                    .filter(|used| used.is_finite() && *used >= 0.0)?;
                let remaining = ((resets_at - now).num_milliseconds() as f64
                    / (f64::from(WEEK_MINUTES) * 60_000.0))
                    .clamp(f64::EPSILON, 1.0);
                Some(Self {
                    pressure: (1.0 - used_percent / 100.0).max(0.0) / remaining,
                    pressure_window: Some(PressureWindow {
                        window: reading.window.clone(),
                        scope: reading.model.clone(),
                        used_percent,
                        resets_at,
                    }),
                })
            })
            .min_by(|left, right| left.pressure.total_cmp(&right.pressure))
            .unwrap_or_default()
    }

    /// `crew`'s pressure: the minimum over the weekly readings of its
    /// account, account-wide or scoped to a model its own contains.
    pub(super) fn for_crew(
        observations: &[ProviderLimitObservation],
        crew: &Crew,
        now: DateTime<Utc>,
    ) -> Self {
        Self::from_readings(
            observations.iter().filter(|reading| {
                reading_covers(
                    &reading.provider,
                    reading.account.as_deref(),
                    reading.model.as_deref(),
                    crew,
                )
            }),
            now,
        )
    }

    /// A raw multiplier with its binding window and reset, suitable for CLI output.
    #[must_use]
    pub fn describe(&self, now: DateTime<Utc>) -> String {
        // Keep enough significant digits to show very small pressures without rounding to zero.
        let value = if self.pressure == 0.0 || (0.01..10000.0).contains(&self.pressure) {
            format!("{:.2}", self.pressure)
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_string()
        } else {
            self.pressure.to_string()
        };
        match &self.pressure_window {
            Some(window) => format!(
                "×{value} ({}{} {}%, resets {})",
                window.window.as_deref().unwrap_or("weekly"),
                window
                    .scope
                    .as_deref()
                    .map(|scope| format!(" [{scope}]"))
                    .unwrap_or_default(),
                window.used_percent,
                short_time(window.resets_at, now)
            ),
            None => format!("×{value} (no live weekly usage)"),
        }
    }
}

/// Display pressure for every enabled crew, including crews without usage readings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrewUsagePressure {
    /// Configured crew name.
    pub crew: String,
    /// Minimum over account-wide and matching model weekly windows.
    #[serde(flatten)]
    pub usage: UsagePressure,
}

/// One read of this host's observations, for weighing crew draws by their
/// crews' weekly pressure.
pub(crate) struct UsagePressureRead {
    observations: Vec<ProviderLimitObservation>,
    now: DateTime<Utc>,
}

impl UsagePressureRead {
    /// `crew`'s pressure at this read.
    pub(crate) fn crew(&self, crew: &Crew) -> UsagePressure {
        UsagePressure::for_crew(&self.observations, crew, self.now)
    }
}

impl OrbitRuntime {
    /// This host's observations at `now`. An unreadable store is logged and
    /// reads as neutral pressure for every crew, as admission treats limits.
    pub(crate) fn usage_pressure_read(&self, now: DateTime<Utc>) -> UsagePressureRead {
        let observations = self
            .provider_limit_observations(now)
            .unwrap_or_else(|error| {
                tracing::warn!(
                    "could not read this host's provider usage for crew pressure: {error}"
                );
                Vec::new()
            });
        UsagePressureRead { observations, now }
    }
}
