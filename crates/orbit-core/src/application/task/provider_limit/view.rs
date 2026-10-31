//! The one read model of this host's provider usage limits [ORB-14698].
//!
//! `orbit run readiness`, `orbit doctor` and the dashboard show provider
//! limits only through [`ProviderLimitsView`], never by reading the store: which
//! readings are live, which keep their crews out of delivery admission, until
//! when, which crews those are, and how old each reading is. Liveness,
//! thresholds and crew coverage are admission's own rules (`provider_limit`),
//! so no surface can disagree with the drain about what is gated. A reading
//! observed on an earlier UTC date is stale even while its reset keeps it live.
//!
//! [ORB-17288] A crew that sets `env` is its own provider account, so its
//! readings, and the status of its account, are listed apart from its
//! provider's default login's, under `provider/crew`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_types::identity::Crew;
use orbit_types::telemetry::{
    ProviderLimitObservation, ProviderLimitSource, USAGE_REPORTING_PROVIDERS,
};
use orbit_types::workflow::{Provider, provider_account_label};
use serde::{Deserialize, Serialize};

use orbit_config::ProviderLimitThreshold;

use super::gate::{reading_at_limit, reading_covers, reading_lifetime};
use super::ledger::PARTIAL_NOTE;
use super::pressure::{CrewUsagePressure, UsagePressure};
use crate::OrbitRuntime;

/// One live reading of a provider usage window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderLimitReading {
    /// Canonical provider name (`claude`, `codex`).
    pub provider: String,
    /// The crew account the reading was taken under, a crew that sets
    /// `env`; `None` for the provider's default login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
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
    /// Seconds from `observed_at` until the view was built. Never negative:
    /// a reading observed in the future reports `0`.
    #[serde(default)]
    pub age_seconds: i64,
    /// Whether `observed_at` falls on an earlier UTC date than the view.
    /// A reading from earlier the same UTC date is fresh.
    #[serde(default)]
    pub stale: bool,
    /// Whether reaching this window stops the account's runs; an overage
    /// window is shown but never gates.
    pub gating: bool,
    /// Whether the reading undercounts: a `usd` budget read from the ledger
    /// leaves out invocations that recorded no cost.
    pub partial: bool,
    /// The used percent at or above which the provider's crews are skipped,
    /// or `off` when `workflow.provider_limit_overrides` turns that gate off.
    /// JSON is the number, or the string `off`.
    pub threshold: ProviderLimitThreshold,
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
    /// `claude five_hour`, `claude [opus] seven_day` for a model-scoped
    /// reading, or `claude/opus-credits 30d budget` for a crew account's.
    #[must_use]
    pub fn label(&self) -> String {
        let scope = self
            .scope
            .as_deref()
            .map(|scope| format!(" [{scope}]"))
            .unwrap_or_default();
        format!(
            "{}{scope} {}",
            provider_account_label(&self.provider, self.account.as_deref()),
            self.window.as_deref().unwrap_or("usage window")
        )
    }

    /// `claude five_hour 93% >= 90% until 15:00Z` for a gated reading,
    /// `claude five_hour 40% (limit 90%)` below a percent threshold, and
    /// `codex primary 100% (gating off)` when the provider's gate is off.
    /// The reset is written as a time of day when it falls on `now`'s UTC date.
    #[must_use]
    pub fn describe(&self, now: DateTime<Utc>) -> String {
        let label = self.label();
        let reported = self.used_percent.map_or_else(
            || "usage not reported".to_string(),
            |used| format!("{used}%"),
        );
        let ProviderLimitThreshold::Percent(threshold) = self.threshold else {
            return format!("{label} {reported}{} (gating off)", self.partial_note());
        };
        if self.gated {
            let usage = match self.used_percent {
                Some(used) if used >= f64::from(threshold) => {
                    format!("{used}% >= {threshold}%")
                }
                Some(used) => format!("exhausted at {used}%"),
                None => "exhausted".to_string(),
            };
            return format!(
                "{label} {usage}{} until {}",
                self.partial_note(),
                short_time(self.until, now)
            );
        }
        if self.gating {
            format!(
                "{label} {reported}{} (limit {threshold}%)",
                self.partial_note()
            )
        } else {
            format!("{label} {reported} (overage window; never gates)")
        }
    }

    /// ` (partial: ...)` for a reading that undercounts, else nothing.
    fn partial_note(&self) -> String {
        if self.partial {
            format!(" ({PARTIAL_NOTE})")
        } else {
            String::new()
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

/// What the host knows about one configured provider account's usage: a
/// provider's default login, or [ORB-17288] a crew that sets `env`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderUsageStatus {
    /// Canonical provider name.
    pub provider: String,
    /// The crew account, for a crew that sets `env`; `None` for the
    /// provider's default login.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// Whether the provider reports its usage windows after a run. One that
    /// does not is read only from a limit failure.
    pub reports_usage: bool,
    /// The provider's gate: a percent, or `off`. JSON is the number or `"off"`.
    pub threshold: ProviderLimitThreshold,
    /// Whether any live reading gates it now.
    pub gated: bool,
    /// Display-only pressure of this account's account-wide weekly windows.
    #[serde(flatten, default)]
    pub usage: UsagePressure,
    /// Its enabled configured crews.
    pub crews: Vec<String>,
}

impl ProviderUsageStatus {
    /// `claude`, or `claude/opus-credits` for a crew account.
    #[must_use]
    pub fn label(&self) -> String {
        provider_account_label(&self.provider, self.account.as_deref())
    }
}

/// The reading closest to its limit among those covering one crew.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CrewProviderLimit {
    pub crew: String,
    pub provider: String,
    /// The crew account the reading belongs to, when the crew sets `env`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub scope: Option<String>,
    pub window: Option<String>,
    pub used_percent: Option<f64>,
    pub exhausted: bool,
    pub resets_at: Option<DateTime<Utc>>,
    /// The crew's provider gate: a percent, or `off`. JSON is the number or `"off"`.
    pub threshold: ProviderLimitThreshold,
    /// Whether admission skips the crew now.
    pub gated: bool,
    pub until: DateTime<Utc>,
}

/// A lane admission does not gate whose crew's provider is at its limit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UngatedLaneLimit {
    /// The setting naming the lane's crew: `workflow.purpose_crews.review`
    /// or `workflow.purpose_crews.pre_landing_review`.
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
    /// Every live reading, by provider, account, scope and window.
    pub readings: Vec<ProviderLimitReading>,
    /// Every provider account an enabled configured crew uses, by provider
    /// and account.
    pub providers: Vec<ProviderUsageStatus>,
    /// Each configured crew some live gating reading covers, by name.
    pub crews: Vec<CrewProviderLimit>,
    /// Display-only weekly pressure for every enabled crew.
    pub crew_pressures: Vec<CrewUsagePressure>,
    /// The review lane, when every member's provider is gated; v1 does not
    /// gate it, so its runs may fail until the limit lifts. System work draws
    /// from the complexity pools, which skip limited crews [ORB-15663].
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
        let policy = runtime.provider_limit_policy();
        let crews = settings.crews().values().collect::<Vec<_>>();
        let mut readings = observations
            .iter()
            .filter_map(|observation| {
                let until = reading_lifetime(observation);
                if until <= now {
                    return None;
                }
                let threshold =
                    policy.account_threshold(&observation.provider, observation.account.as_deref());
                let gated = match threshold {
                    ProviderLimitThreshold::Off => false,
                    ProviderLimitThreshold::Percent(percent) => {
                        observation.gating && reading_at_limit(observation, percent)
                    }
                };
                Some(ProviderLimitReading {
                    provider: canonical_provider(&observation.provider),
                    account: observation.account.clone(),
                    scope: observation.model.clone(),
                    window: observation.window.clone(),
                    used_percent: observation.used_percent,
                    exhausted: observation.exhausted,
                    resets_at: observation.resets_at,
                    source: observation.source,
                    observed_at: observation.observed_at,
                    age_seconds: (now - observation.observed_at).num_seconds().max(0),
                    stale: observation.observed_at.date_naive() < now.date_naive(),
                    gating: observation.gating,
                    partial: observation.partial,
                    threshold,
                    gated,
                    until,
                    crews: crews
                        .iter()
                        .filter(|crew| {
                            crew.enabled
                                && reading_covers(
                                    &observation.provider,
                                    observation.account.as_deref(),
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
            (&left.provider, &left.account, &left.scope, &left.window).cmp(&(
                &right.provider,
                &right.account,
                &right.scope,
                &right.window,
            ))
        });

        let mut providers = BTreeMap::<(String, Option<String>), BTreeSet<String>>::new();
        for crew in crews.iter().filter(|crew| crew.enabled) {
            providers
                .entry((
                    canonical_provider(&crew.assignment.provider),
                    crew.account().map(ToOwned::to_owned),
                ))
                .or_default()
                .insert(crew.name.clone());
        }
        let providers = providers
            .into_iter()
            .map(|((provider, account), crews)| ProviderUsageStatus {
                usage: UsagePressure::from_readings(
                    observations.iter().filter(|reading| {
                        canonical_provider(&reading.provider) == provider
                            && reading.account == account
                            && reading.model.is_none()
                    }),
                    now,
                ),
                reports_usage: USAGE_REPORTING_PROVIDERS.contains(&provider.as_str()),
                threshold: policy.account_threshold(&provider, account.as_deref()),
                gated: readings.iter().any(|reading| {
                    reading.gated && reading.provider == provider && reading.account == account
                }),
                crews: crews.into_iter().collect(),
                provider,
                account,
            })
            .collect();

        let crew_pressures = crews
            .iter()
            .filter(|crew| crew.enabled)
            .map(|crew| CrewUsagePressure {
                crew: crew.name.clone(),
                usage: UsagePressure::for_crew(observations, crew, now),
            })
            .collect();

        let crew_limits = crews
            .iter()
            .filter_map(|crew| tightest(&readings, crew))
            .collect();

        let lane_limit = |setting: &'static str, name: &str| {
            let crew = settings.crews().get(name.trim())?;
            let until = readings
                .iter()
                .filter(|reading| {
                    reading.gated
                        && reading_covers(
                            &reading.provider,
                            reading.account.as_deref(),
                            reading.scope.as_deref(),
                            crew,
                        )
                })
                .map(|reading| reading.until)
                .max()?;
            Some(UngatedLaneLimit {
                setting,
                crew: crew.name.clone(),
                provider: canonical_provider(&crew.assignment.provider),
                until,
            })
        };
        // A review pool skips its limited members, so it is held only when
        // every member is [ORB-15195]. The pre-landing pool is a lane of its
        // own only while it is set; unset, the review pool is that lane too.
        let policy = runtime.operation_policy();
        let mut review_pools = vec![(
            orbit_config::REVIEW_CREW_KEY,
            policy.review_crew.value.as_slice(),
        )];
        let pre_landing = policy.pre_landing_review_pool();
        if !pre_landing.is_fallback() {
            review_pools.push((pre_landing.key, pre_landing.entries));
        }
        let ungated_lanes = review_pools
            .into_iter()
            .flat_map(|(setting, pool)| {
                let limits: Vec<UngatedLaneLimit> = pool
                    .iter()
                    .filter_map(|entry| {
                        let name = entry
                            .split_once(':')
                            .map_or(entry.as_str(), |(name, _)| name);
                        lane_limit(setting, name)
                    })
                    .collect();
                if !pool.is_empty() && limits.len() == pool.len() {
                    limits
                } else {
                    Vec::new()
                }
            })
            .collect();

        Self {
            as_of: now,
            readings,
            providers,
            crews: crew_limits,
            crew_pressures,
            ungated_lanes,
            error: None,
        }
    }

    /// The limit shown for `crew`, when a live gating reading covers it.
    #[must_use]
    pub fn crew(&self, crew: &str) -> Option<&CrewProviderLimit> {
        self.crews.iter().find(|limit| limit.crew == crew)
    }

    /// The live readings of `provider`'s `account`: its default login's for
    /// `None`.
    pub fn provider_readings<'a>(
        &'a self,
        provider: &'a str,
        account: Option<&'a str>,
    ) -> impl Iterator<Item = &'a ProviderLimitReading> {
        self.readings.iter().filter(move |reading| {
            reading.provider == provider && reading.account.as_deref() == account
        })
    }
}

impl OrbitRuntime {
    /// This host's provider limits at `now`. An unreadable store is reported
    /// in [`ProviderLimitsView::error`] rather than failing the caller, as
    /// admission treats it.
    #[must_use]
    pub fn provider_limits_view(&self, now: DateTime<Utc>) -> ProviderLimitsView {
        match self.provider_limit_observations(now) {
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
            reading.gating
                && reading_covers(
                    &reading.provider,
                    reading.account.as_deref(),
                    reading.scope.as_deref(),
                    crew,
                )
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
        account: reading.account.clone(),
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
