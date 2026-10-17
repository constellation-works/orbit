//! The crews a pull drain's window can run [ORB-13941], and what a claimed
//! leaf's settlement phase means for the operator.

use chrono::{DateTime, Duration, Utc};
use orbit_store::contracts::{AdmissionCrewCapability, LocalPullPhase};
use orbit_types::workflow::{CrewExclusion, CrewExclusionSource, PullAuthExclusion};
use serde::Serialize;

use super::entries::RefusedPullSettlement;

/// The crews a pull drain's window can run, and those it will not [ORB-13941].
///
/// The window's provider preflight, taken once when it opened, minus every
/// crew a claimed leaf of this drain found unusable since — an authentication
/// failure excludes every configured crew of that provider for the window, a
/// capacity failure only the leaf's crew and only for a bounded backoff
/// [ORB-15791] — and, when the drain was submitted with `--allow-crew`,
/// outside that restriction [ORB-14174]. Derived from the drain's run input
/// and its own admission records, so it survives a follower restart and a
/// resume, and ends with the drain.
///
/// [ORB-14697] Each pass also excludes the crews this host's provider-limit
/// store finds at their usage limit, until the reading lapses. The reason
/// names the host whose local store supplied the reading; an owner's reading
/// does not gate a follower. A timed
/// exclusion — a provider limit or a capacity backoff — carries its `until`,
/// and the crew is runnable again within the same window once it passes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PullCrewWindow {
    /// When the preflight ran; `None` while the drain has not taken one.
    pub checked_at: Option<DateTime<Utc>>,
    /// Crews the window can run claimed work as. `None` without a preflight
    /// or restriction: then every crew not excluded is offered to the owner.
    pub runnable: Option<Vec<String>>,
    /// The drain's `--allow-crew` restriction, by canonical registry name;
    /// `None` when it runs every crew the window can.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed: Option<Vec<String>>,
    /// The crew a task naming none runs as on this host.
    pub default_crew: Option<String>,
    /// Currently excluded crews and reasons; auth exclusions may recover.
    pub excluded: Vec<CrewExclusion>,
    /// Active provider auth failures and their delayed recovery schedule.
    pub auth_exclusions: Vec<PullAuthExclusion>,
    /// Why this host claims nothing more for the rest of the window: a
    /// claimed leaf of this drain was released for a failure of the host
    /// itself, whatever its crew [ORB-14257]. `None` while the host runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_suppressed: Option<String>,
    /// Crews the window can run once its timed exclusions lift, before
    /// `allowed` narrows them. The owner's reviewer runs as one of these:
    /// the restriction selects the implementation crews a claim may carry,
    /// not the review it owes.
    #[serde(skip)]
    pub(super) reviewable: Option<Vec<String>>,
}

impl PullCrewWindow {
    /// What the drain declares to the owner on each request.
    #[must_use]
    pub fn capability(&self) -> AdmissionCrewCapability {
        AdmissionCrewCapability {
            runnable: self.runnable.clone(),
            default_crew: self.default_crew.clone(),
            excluded: self.excluded.clone(),
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
        if let Some(exclusion) = excluded.iter().find(|exclusion| exclusion.crew == crew)
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
        }
        .unrunnable_reason(Some(crew))
    }

    /// Whether the window can run nothing at all: the host is suppressed,
    /// or every configured crew is excluded, so requesting work would only
    /// collect idle receipts.
    #[must_use]
    pub fn runs_nothing(&self) -> bool {
        self.host_suppressed.is_some() || self.runnable.as_ref().is_some_and(Vec::is_empty)
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
        let candidate_crews: Vec<&str> = match (&self.allowed, &self.reviewable) {
            (Some(allowed), Some(reviewable)) => reviewable
                .iter()
                .filter(|crew| allowed.iter().any(|a| a == *crew))
                .map(String::as_str)
                .collect(),
            (None, Some(reviewable)) => reviewable.iter().map(String::as_str).collect(),
            (Some(allowed), None) => allowed
                .iter()
                .filter(|crew| {
                    !self
                        .excluded
                        .iter()
                        .any(|exclusion| &exclusion.crew == *crew && exclusion.until.is_none())
                })
                .map(String::as_str)
                .collect(),
            (None, None) => Vec::new(),
        };
        !candidate_crews.is_empty()
            && candidate_crews.iter().all(|crew| {
                self.excluded
                    .iter()
                    .any(|exclusion| exclusion.crew == *crew && exclusion.until.is_some())
            })
    }

    /// One line per excluded crew, for a terminal report.
    #[must_use]
    pub fn describe(&self) -> Vec<String> {
        let mut lines = Vec::with_capacity(self.excluded.len() + 3);
        if let Some(reason) = &self.host_suppressed {
            lines.push(format!("host suppressed: {reason}"));
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
    serde_json::to_value(phase)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}
