//! The one read model of this host's provider usage limits [ORB-14698].
//!
//! `orbit run readiness`, `orbit doctor` and the dashboard show provider
//! limits only through [`ProviderLimitsView`], never by reading the store: which
//! readings are live, which keep their crews out of delivery admission, until
//! when, and which crews those are. Liveness, thresholds and crew coverage are
//! admission's own rules (`provider_limit`), so no surface can disagree with
//! the drain about what is gated.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_types::identity::Crew;
use orbit_types::telemetry::{
    ProviderLimitObservation, ProviderLimitSource, USAGE_REPORTING_PROVIDERS,
};
use orbit_types::workflow::Provider;
use serde::{Deserialize, Serialize};

use super::provider_limit::{reading_at_limit, reading_covers, reading_lifetime};
use crate::OrbitRuntime;

/// One live reading of a provider usage window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderLimitReading {
    /// Canonical provider name (`claude`, `codex`).
    pub provider: String,
    /// The model or model family the reading covers; `None` covers the whole
    /// account.
    pub scope: Option<String>,
    /// The window as the provider labels it (`five_hour`), when known.
    pub window: Option<String>,
    /// How much of the window is used, in percent; `None` when the provider
    /// did not say, as a limit failure usually does not.
    pub used_percent: Option<f64>,
    /// Whether the provider reported the limit reached.
    pub exhausted: bool,
    /// When the provider said the window resets.
    pub resets_at: Option<DateTime<Utc>>,
    pub source: ProviderLimitSource,
    pub observed_at: DateTime<Utc>,
    /// Whether reaching this window stops the account's runs; an overage
    /// window is shown but never gates.
    pub gating: bool,
    /// The used percent at or above which the provider's crews are skipped
    /// (`workflow.provider_limit_*`).
    pub threshold: u8,
    /// Whether the reading keeps the crews it covers out of delivery
    /// admission now.
    pub gated: bool,
    /// When the reading stops counting: its reset, or the lifetime admission
    /// gives a reading that named none.
    pub until: DateTime<Utc>,
    /// The enabled configured crews the reading covers. Admission skips them
    /// while `gated`.
    pub crews: Vec<String>,
}

impl ProviderLimitReading {
    /// `claude five_hour`, or `claude [opus] seven_day` for a model-scoped
    /// reading.
    #[must_use]
    pub fn label(&self) -> String {
        let scope = self
            .scope
            .as_deref()
            .map(|scope| format!(" [{scope}]"))
            .unwrap_or_default();
        format!(
            "{}{scope} {}",
            self.provider,
            self.window.as_deref().unwrap_or("usage window")
        )
    }

    /// `claude five_hour 93% >= 90% until 15:00Z` for a gated reading,
    /// `claude five_hour 40% (limit 90%)` otherwise. The reset is written
    /// as a time of day when it falls on `now`'s UTC date.
    #[must_use]
    pub fn describe(&self, now: DateTime<Utc>) -> String {
        let label = self.label();
        if self.gated {
            let usage = match self.used_percent {
                Some(used) if used >= f64::from(self.threshold) => {
                    format!("{used}% >= {}%", self.threshold)
                }
                Some(used) => format!("exhausted at {used}%"),
                None => "exhausted".to_string(),
            };
            return format!("{label} {usage} until {}", short_time(self.until, now));
        }
        let usage = self.used_percent.map_or_else(
            || "usage not reported".to_string(),
            |used| format!("{used}%"),
        );
        if self.gating {
            format!("{label} {usage} (limit {}%)", self.threshold)
        } else {
            format!("{label} {usage} (overage window; never gates)")
        }
    }

    /// `<describe>: opus, sonnet skipped`, the line a gated reading prints.
    #[must_use]
    pub fn skipped_line(&self, now: DateTime<Utc>) -> String {
        let crews = if self.crews.is_empty() {
            "no configured crew".to_string()
        } else {
            self.crews.join(", ")
        };
        format!("{}: {crews} skipped", self.describe(now))
    }
}

/// What the host knows about one configured provider's usage.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProviderUsageStatus {
    /// Canonical provider name.
    pub provider: String,
    /// Whether the provider reports its usage windows after a run. One that
    /// does not is read only from a limit failure.
    pub reports_usage: bool,
    pub threshold: u8,
    /// Whether any live reading gates it now.
    pub gated: bool,
    /// Its enabled configured crews.
    pub crews: Vec<String>,
}

/// The reading closest to its limit among those covering one crew.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CrewProviderLimit {
    pub crew: String,
    pub provider: String,
    pub scope: Option<String>,
    pub window: Option<String>,
    pub used_percent: Option<f64>,
    pub exhausted: bool,
    pub resets_at: Option<DateTime<Utc>>,
    pub threshold: u8,
    /// Whether admission skips the crew now.
    pub gated: bool,
    pub until: DateTime<Utc>,
}

/// A lane admission does not gate whose crew's provider is at its limit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UngatedLaneLimit {
    /// The setting naming the lane's crew: `workflow.system_crew` or
    /// `operation.review_crew`.
    pub setting: &'static str,
    pub crew: String,
    pub provider: String,
    /// When the limit on its provider lifts.
    pub until: DateTime<Utc>,
}

/// This host's provider limits, as every surface shows them.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProviderLimitsView {
    pub as_of: DateTime<Utc>,
    /// Every live reading, by provider, scope and window.
    pub readings: Vec<ProviderLimitReading>,
    /// Every provider an enabled configured crew uses, by name.
    pub providers: Vec<ProviderUsageStatus>,
    /// Each configured crew some live gating reading covers, by name.
    pub crews: Vec<CrewProviderLimit>,
    /// The system and review lanes whose provider is gated; v1 does not gate
    /// them, so their runs may fail until the limit lifts.
    pub ungated_lanes: Vec<UngatedLaneLimit>,
    /// Why the store could not be read. Admission then gates nothing, and
    /// the view shows no reading.
    pub error: Option<String>,
}

impl ProviderLimitsView {
    /// The view of `observations` under `runtime`'s configuration at `now`.
    fn build(
        runtime: &OrbitRuntime,
        observations: &[ProviderLimitObservation],
        now: DateTime<Utc>,
    ) -> Self {
        let settings = runtime.context.settings();
        let policy = settings.provider_limit();
        // The synthesized `system` entry mirrors another crew; it is that
        // crew, not a second one.
        let crews = settings
            .crews()
            .values()
            .filter(|crew| !(crew.name == "system" && settings.system_crew_alias().is_some()))
            .collect::<Vec<_>>();
        let mut readings = observations
            .iter()
            .filter_map(|observation| {
                let until = reading_lifetime(observation);
                if until <= now {
                    return None;
                }
                let threshold = policy.threshold(&observation.provider);
                Some(ProviderLimitReading {
                    provider: canonical_provider(&observation.provider),
                    scope: observation.model.clone(),
                    window: observation.window.clone(),
                    used_percent: observation.used_percent,
                    exhausted: observation.exhausted,
                    resets_at: observation.resets_at,
                    source: observation.source,
                    observed_at: observation.observed_at,
                    gating: observation.gating,
                    threshold,
                    gated: observation.gating && reading_at_limit(observation, threshold),
                    until,
                    crews: crews
                        .iter()
                        .filter(|crew| {
                            crew.enabled
                                && reading_covers(
                                    &observation.provider,
                                    observation.model.as_deref(),
                                    crew,
                                )
                        })
                        .map(|crew| crew.name.clone())
                        .collect(),
                })
            })
            .collect::<Vec<_>>();
        readings.sort_by(|left, right| {
            (&left.provider, &left.scope, &left.window).cmp(&(
                &right.provider,
                &right.scope,
                &right.window,
            ))
        });

        let mut providers = BTreeMap::<String, BTreeSet<String>>::new();
        for crew in crews.iter().filter(|crew| crew.enabled) {
            providers
                .entry(canonical_provider(&crew.assignment.provider))
                .or_default()
                .insert(crew.name.clone());
        }
        let providers = providers
            .into_iter()
            .map(|(provider, crews)| ProviderUsageStatus {
                reports_usage: USAGE_REPORTING_PROVIDERS.contains(&provider.as_str()),
                threshold: policy.threshold(&provider),
                gated: readings
                    .iter()
                    .any(|reading| reading.gated && reading.provider == provider),
                crews: crews.into_iter().collect(),
                provider,
            })
            .collect();

        let crew_limits = crews
            .iter()
            .filter_map(|crew| tightest(&readings, crew))
            .collect();

        let review_crew = runtime.operation_policy().review_crew.value.clone();
        let ungated_lanes = [
            (
                "workflow.system_crew",
                Some(settings.system_crew().to_string()),
            ),
            ("operation.review_crew", review_crew),
        ]
        .into_iter()
        .filter_map(|(setting, name)| {
            let crew = settings.crews().get(name?.trim())?;
            let until = readings
                .iter()
                .filter(|reading| {
                    reading.gated
                        && reading_covers(&reading.provider, reading.scope.as_deref(), crew)
                })
                .map(|reading| reading.until)
                .max()?;
            Some(UngatedLaneLimit {
                setting,
                crew: crew.name.clone(),
                provider: canonical_provider(&crew.assignment.provider),
                until,
            })
        })
        .collect();

        Self {
            as_of: now,
            readings,
            providers,
            crews: crew_limits,
            ungated_lanes,
            error: None,
        }
    }

    /// The limit shown for `crew`, when a live gating reading covers it.
    #[must_use]
    pub fn crew(&self, crew: &str) -> Option<&CrewProviderLimit> {
        self.crews.iter().find(|limit| limit.crew == crew)
    }

    /// The live readings of `provider`.
    pub fn provider_readings<'a>(
        &'a self,
        provider: &'a str,
    ) -> impl Iterator<Item = &'a ProviderLimitReading> {
        self.readings
            .iter()
            .filter(move |reading| reading.provider == provider)
    }
}

impl OrbitRuntime {
    /// This host's provider limits at `now`. An unreadable store is reported
    /// in [`ProviderLimitsView::error`] rather than failing the caller, as
    /// admission treats it.
    #[must_use]
    pub fn provider_limits_view(&self, now: DateTime<Utc>) -> ProviderLimitsView {
        match self.provider_limits() {
            Ok(observations) => ProviderLimitsView::build(self, &observations, now),
            Err(error) => ProviderLimitsView {
                error: Some(error.to_string()),
                ..ProviderLimitsView::build(self, &[], now)
            },
        }
    }
}

/// The gating reading covering `crew` closest to its limit: the gated one
/// lasting longest, else the most used.
fn tightest(readings: &[ProviderLimitReading], crew: &Crew) -> Option<CrewProviderLimit> {
    let covering = readings
        .iter()
        .filter(|reading| {
            reading.gating && reading_covers(&reading.provider, reading.scope.as_deref(), crew)
        })
        .collect::<Vec<_>>();
    let reading = covering
        .iter()
        .filter(|reading| reading.gated)
        .max_by_key(|reading| reading.until)
        .or_else(|| {
            covering.iter().max_by(|left, right| {
                let used = |reading: &ProviderLimitReading| {
                    if reading.exhausted {
                        f64::INFINITY
                    } else {
                        reading.used_percent.unwrap_or(0.0)
                    }
                };
                used(left).total_cmp(&used(right))
            })
        })?;
    Some(CrewProviderLimit {
        crew: crew.name.clone(),
        provider: reading.provider.clone(),
        scope: reading.scope.clone(),
        window: reading.window.clone(),
        used_percent: reading.used_percent,
        exhausted: reading.exhausted,
        resets_at: reading.resets_at,
        threshold: reading.threshold,
        gated: reading.gated,
        until: reading.until,
    })
}

fn canonical_provider(provider: &str) -> String {
    Provider::parse(provider).map_or_else(
        |_| provider.to_string(),
        |parsed| parsed.as_str().to_string(),
    )
}

/// `15:00Z` on `now`'s UTC date, `2026-10-10 15:00Z` otherwise.
#[must_use]
pub fn short_time(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    if at.date_naive() == now.date_naive() {
        at.format("%H:%MZ").to_string()
    } else {
        at.format("%Y-%m-%d %H:%MZ").to_string()
    }
}
