//! The crews a pull drain's window can run [ORB-13941], and what a claimed
//! leaf's settlement phase means for the operator.

pub use crate::application::contracts::{OwnerBusyBackoff, PullCrewWindow};
use chrono::{DateTime, Duration, Utc};
use orbit_store::contracts::{AdmissionCrewCapability, LocalPullPhase};
use orbit_types::identity::same_crew_family;
use orbit_types::workflow::{CrewExclusion, CrewExclusionSource, PullAuthExclusion};

use super::entries::RefusedPullSettlement;

impl PullCrewWindow {
    /// What the drain declares to the owner on each request.
    #[must_use]
    pub fn capability(&self) -> AdmissionCrewCapability {
        AdmissionCrewCapability {
            runnable: self.runnable.clone(),
            default_crew: self.default_crew.clone(),
            excluded: self.excluded.clone(),
            usage_pressure: self.usage_pressure.clone(),
        }
    }

    /// Why the owner's reviewer crew cannot run in this window, or `None`
    /// when it can. Judged without `allowed`: only exclusions and the
    /// preflight count, and a provider limit does not hold review. A
    /// capacity backoff does, and the reason names when it lifts [ORB-15791].
    #[must_use]
    pub fn reviewer_unrunnable_reason(&self, crew: &str) -> Option<String> {
        let excluded: Vec<CrewExclusion> = self
            .excluded
            .iter()
            .filter(|exclusion| exclusion.source != CrewExclusionSource::ProviderLimit)
            .cloned()
            .collect();
        if let Some(exclusion) = excluded
            .iter()
            .find(|exclusion| same_crew_family(&exclusion.crew, crew))
            && let Some(until) = exclusion.until
        {
            return Some(format!(
                "crew `{crew}` is backed off on the executor until {}, when requests resume: {}",
                until.to_rfc3339(),
                exclusion.reason
            ));
        }
        AdmissionCrewCapability {
            runnable: self.reviewable.clone(),
            default_crew: None,
            excluded,
            usage_pressure: Vec::new(),
        }
        .unrunnable_reason(Some(crew))
    }

    /// Whether the window can run nothing at all: the host is suppressed,
    /// the drain is backing off a busy owner, or every configured crew is
    /// excluded, so requesting work would only collect idle receipts.
    #[must_use]
    pub fn runs_nothing(&self) -> bool {
        self.host_suppressed.is_some()
            || self.owner_busy_backoff.is_some()
            || self.runnable.as_ref().is_some_and(Vec::is_empty)
    }

    /// Whether every crew this drain could otherwise run — the
    /// allowlist-permitted preflight-runnable set — is excluded until a set
    /// time, by a provider usage limit or a capacity backoff, so this drain
    /// admits again once one lifts [ORB-14697, ORB-14902, ORB-15791].
    #[must_use]
    pub fn held_until_lift(&self) -> bool {
        if self.host_suppressed.is_some() || self.runnable.as_ref().is_some_and(|r| !r.is_empty()) {
            return false;
        }
        let candidate_crews = self.permitted.as_deref().unwrap_or_default();
        !candidate_crews.is_empty()
            && candidate_crews.iter().all(|crew| {
                self.excluded.iter().any(|exclusion| {
                    same_crew_family(&exclusion.crew, crew) && exclusion.until.is_some()
                })
            })
    }

    /// One line per excluded crew, for a terminal report.
    #[must_use]
    pub fn describe(&self) -> Vec<String> {
        let mut lines = Vec::with_capacity(self.excluded.len() + 3);
        if let Some(reason) = &self.host_suppressed {
            lines.push(format!("host suppressed: {reason}"));
        }
        if let Some(backoff) = &self.owner_busy_backoff {
            lines.push(format!(
                "owner busy: backed off until {} ({} of {OWNER_BUSY_EPISODE_LIMIT} episodes): {}",
                backoff.until.to_rfc3339(),
                backoff.episodes,
                backoff.reason
            ));
        }
        if let Some(allowed) = &self.allowed {
            lines.push(format!("allowed (--allow-crew): {}", allowed.join(", ")));
        }
        if let Some(runnable) = &self.runnable {
            lines.push(if runnable.is_empty() {
                "runnable: none".to_string()
            } else {
                format!("runnable: {}", runnable.join(", "))
            });
        }
        lines.extend(self.excluded.iter().map(|exclusion| {
            let source = match exclusion.source {
                CrewExclusionSource::Preflight => "preflight",
                CrewExclusionSource::ProviderUnavailable => "provider_unavailable",
                CrewExclusionSource::LeafReleased => "leaf_released",
                CrewExclusionSource::ProviderLimit => "provider_limit",
            };
            match exclusion.until {
                Some(until) => format!(
                    "excluded {} ({source} until {}): {}",
                    exclusion.crew,
                    until.to_rfc3339(),
                    exclusion.reason
                ),
                None => format!(
                    "excluded {} ({source}): {}",
                    exclusion.crew, exclusion.reason
                ),
            }
        }));
        lines.extend(self.auth_exclusions.iter().map(PullAuthExclusion::describe));
        lines
    }
}

/// How long a drain requests no claim after a claimed leaf was released for
/// the owner's busy store. Long enough for the owner's lock holder to finish,
/// short against a drain's window.
const OWNER_BUSY_BACKOFF_MINUTES: i64 = 5;

/// How many separate busy-owner episodes in one window a drain backs off
/// before it suppresses its host for the rest of the window. Releases that
/// end within [`OWNER_BUSY_BACKOFF_MINUTES`] of the previous one belong to the
/// same episode — leaves already running when the owner's store locked, which
/// one backoff covers — so only an owner that stays busy across backoffs
/// reaches the bound.
pub(crate) const OWNER_BUSY_EPISODE_LIMIT: u32 = 3;

/// Whether a drain's busy-owner releases hold it briefly or suppress its
/// host for the rest of the window.
pub(super) enum OwnerBusyVerdict {
    Backoff(OwnerBusyBackoff),
    Suppress(String),
}

/// The releases for the owner's busy store that one pull drain's claimed
/// leaves met in its window.
#[derive(Debug, Default)]
pub(super) struct OwnerBusyReleases {
    releases: Vec<(DateTime<Utc>, String)>,
}

impl OwnerBusyReleases {
    pub(super) fn record(&mut self, at: DateTime<Utc>, reason: String) {
        self.releases.push((at, reason));
    }

    /// The backoff from the latest release, or the host's suppression once
    /// the releases span [`OWNER_BUSY_EPISODE_LIMIT`] episodes; `None`
    /// without a release.
    pub(super) fn verdict(mut self) -> Option<OwnerBusyVerdict> {
        self.releases.sort_by_key(|(at, _)| *at);
        let backoff = Duration::minutes(OWNER_BUSY_BACKOFF_MINUTES);
        let mut episodes = 0_u32;
        let mut previous: Option<DateTime<Utc>> = None;
        for (at, _) in &self.releases {
            if previous.is_none_or(|previous| *at >= previous + backoff) {
                episodes += 1;
            }
            previous = Some(*at);
        }
        let releases = u32::try_from(self.releases.len()).unwrap_or(u32::MAX);
        let (at, reason) = self.releases.pop()?;
        if episodes >= OWNER_BUSY_EPISODE_LIMIT {
            return Some(OwnerBusyVerdict::Suppress(format!(
                "the owner's store stayed locked across {episodes} backoffs in this window \
                 ({releases} releases; the bound is {OWNER_BUSY_EPISODE_LIMIT}); latest: {reason}"
            )));
        }
        Some(OwnerBusyVerdict::Backoff(OwnerBusyBackoff {
            until: at + backoff,
            episodes,
            releases,
            reason,
        }))
    }
}

/// [ORB-15791] How long a crew's first capacity failure in a window backs it
/// off; each further one in the same window doubles it, up to
/// [`CAPACITY_BACKOFF_CAP_MINUTES`], well below a drain's window.
const CAPACITY_BACKOFF_MINUTES: i64 = 10;
const CAPACITY_BACKOFF_CAP_MINUTES: i64 = 60;

/// The capacity failures one crew's claimed leaves met in a pull drain's
/// window.
#[derive(Debug, Default)]
pub(super) struct CapacityFailures {
    count: u32,
    latest: Option<(DateTime<Utc>, String)>,
}

impl CapacityFailures {
    pub(super) fn record(&mut self, at: DateTime<Utc>, reason: String) {
        self.count += 1;
        if self.latest.as_ref().is_none_or(|(latest, _)| at >= *latest) {
            self.latest = Some((at, reason));
        }
    }

    /// When the backoff from the latest failure lifts, and that failure's
    /// reason.
    pub(super) fn backoff(self) -> Option<(DateTime<Utc>, String)> {
        let (at, reason) = self.latest?;
        let doublings = self.count.saturating_sub(1).min(8);
        let minutes = (CAPACITY_BACKOFF_MINUTES << doublings).min(CAPACITY_BACKOFF_CAP_MINUTES);
        Some((at + Duration::minutes(minutes), reason))
    }
}

pub(super) fn phase_guidance(
    phase: LocalPullPhase,
    refusal: Option<&str>,
    refused: Option<&RefusedPullSettlement>,
) -> String {
    use LocalPullPhase as Phase;
    if let Some(refused) = refused {
        return format!(
            "refused by the owner ({}); {}",
            refused.reason, refused.remedy
        );
    }
    match (phase, refusal) {
        (Phase::Settled, Some(refusal)) => format!(
            "the owner had already ended this claim ({refusal}); decide the task's status on \
             the owner"
        ),
        (Phase::Settled, None) => "the outcome was delivered to the owner".to_string(),
        (Phase::Settling, _) => "the outcome is recorded but not delivered to the owner; \
             the clock sweep retries delivery, as does `orbit run auto --stop`"
            .to_string(),
        (Phase::Launched, _) => {
            "the leaf settles itself when it ends; nothing to do while it runs".to_string()
        }
        (Phase::Launching, _) => "the launch was never acknowledged; recover the claim on the \
             owner's dashboard rather than starting another attempt"
            .to_string(),
        _ => "admitted, not launched yet".to_string(),
    }
}

pub(super) fn phase_name(phase: LocalPullPhase) -> String {
    orbit_common::protocol::serde_string_label(&phase).unwrap_or_else(|| "unknown".to_string())
}
