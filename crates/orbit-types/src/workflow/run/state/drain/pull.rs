//! Pull-drain exclusions, recovery and admission checkpoints.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A live operator request to stop a bounded drain's new admissions [ORB-11283].
///
/// This is not cancellation. The coordinator stays the same run, already
/// admitted children keep their completion authority, and the admission path
/// treats the flag as "offer nothing" on the next pass. Cancellation of those
/// children is a separate, explicit `orbit run cancel` of each child run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainAdmissionsStop {
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub stopped_at: DateTime<Utc>,
}

/// Why a follower cannot run a crew for the rest of its pull drain window, or
/// until a provider usage limit or a capacity backoff lifts.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CrewExclusionSource {
    /// The window's provider preflight: the crew is disabled, or its
    /// provider CLI cannot be found or resolved here.
    Preflight,
    /// A claimed leaf failed because the provider could not be used.
    /// Authentication excludes every configured crew of that provider for
    /// the window; a capacity failure excludes only the leaf's crew, and only
    /// until the exclusion's `until`: a bounded backoff that doubles with each
    /// repeat in the window [ORB-15791].
    ProviderUnavailable,
    /// A claimed leaf on this crew was released for a failure class that
    /// [excludes its crew](crate::workflow::ClaimFailureClass::excludes_crew) [ORB-14257].
    LeafReleased,
    /// The host's latest reading of the crew's provider usage window is at
    /// or above the configured threshold, or exhausted. Like a capacity
    /// backoff it lasts only until the exclusion's `until`, so the crew is
    /// runnable again within the same window.
    ProviderLimit,
}

/// One crew a follower will not run, and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct CrewExclusion {
    pub crew: String,
    pub source: CrewExclusionSource,
    pub reason: String,
    /// When a timed exclusion lifts — a [`CrewExclusionSource::ProviderLimit`]
    /// or a capacity [`CrewExclusionSource::ProviderUnavailable`] — so the
    /// crew is runnable again in the same window; `None` for an exclusion
    /// that lasts the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub until: Option<DateTime<Utc>>,
}

/// Operator-visible authentication exclusion for one provider on this host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullAuthExclusion {
    pub provider: String,
    pub host: String,
    pub excluded_at: DateTime<Utc>,
    pub error_class: String,
    pub relogin_hint: String,
    pub credential_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_probe_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub attempts: u32,
}

impl PullAuthExclusion {
    /// Shared human-readable warning for doctor and run show.
    pub fn describe(&self) -> String {
        format!(
            "{} auth failed on {} at {} ({}; credentials: {}); {}; next probe: {}",
            self.provider,
            self.host,
            self.excluded_at.to_rfc3339(),
            self.error_class,
            self.credential_source,
            self.relogin_hint,
            self.next_probe_at.map_or_else(
                || "not declared; start a new drain after re-login".into(),
                |at| at.to_rfc3339()
            ),
        )
    }
}

/// Durable probe backoff and recovery for a released authentication incident.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullAuthRecovery {
    pub exclusion: PullAuthExclusion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovered_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_source: Option<String>,
}

/// The crews a pull drain's window can run, from the provider preflight it
/// took when the window opened [ORB-13941]. Cached for the window: a
/// newly installed CLI takes effect with the next drain. Auth failures may
/// recover within the window through a declared probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullCrewPreflight {
    pub checked_at: DateTime<Utc>,
    /// Configured crews this host can run, by registry name.
    pub runnable: Vec<String>,
    /// The crew a task naming none runs as here (`workflow.default_crew`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_crew: Option<String>,
    /// Configured crews the preflight excluded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<CrewExclusion>,
}

/// The one admission pass a pull drain submitted without a window is
/// authorized for [ORB-14174].
///
/// A pull drain with `for_seconds` zero runs one bounded admission pass and
/// then only settles. Its window is already expired by then, so the window
/// cannot tell that pass apart from a timed window that ran out; this marker
/// does. It is recorded before the pass sends its first request, so a retried
/// or resumed run (resume carries run state) sees the pass consumed and never
/// requests again, while the requests the pass recorded are still carried to
/// settlement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullSinglePass {
    pub consumed_at: DateTime<Utc>,
}

/// The pass that closed a timed pull drain's window for good [ORB-15417].
///
/// A pull drain keeps passing after its window lapses, until its leaves
/// settle, and an operator may extend the window during that tail. The pass
/// that finds the window still closed and nothing left to settle ends the
/// drain; it records this in the same transaction that rereads the deadline,
/// so an extension accepted first keeps the drain admitting and one requested
/// afterwards is refused instead of reported as accepted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullWindowClose {
    pub closed_at: DateTime<Utc>,
}

/// What a pull drain's host load forecast had room for when it last
/// requested work: each kind of work it would admit for its profile's whole
/// horizon, each it would not and why, and whether its request carried the
/// list. Shadow mode records it and requests as with `off`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullAdmissionFit {
    /// `shadow` or `enforce`.
    pub mode: String,
    pub computed_at: DateTime<Utc>,
    /// Whether the request carried the list: under `enforce` only.
    pub sent: bool,
    /// The work that fits: `change/hard`, `no-diff/unset`,
    /// `auto-task:<template>` for a template the host has a profile of, or
    /// `auto-task:*/change` and `auto-task:*/no-diff` for a template it has
    /// none of, by its base class.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub admissible: Vec<String>,
    /// The work that does not.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unfit: Vec<PullUnfitWork>,
}

/// Work a pull drain's forecast had no room for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullUnfitWork {
    /// As in [`PullAdmissionFit::admissible`].
    pub work: String,
    /// The first minute and resource over its limit, say `cpu 94% at +6m`.
    pub reason: String,
}

/// A pull drain's open failure breaker [ORB-15639]: the claims it admitted
/// last settled as consecutive failures, so it requests no new work until
/// `half_open_at`. From then it is half-open and admits one probe claim at a
/// time; a probe that succeeds closes it, and one that fails opens it again
/// with a longer wait. Absent while the breaker is closed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullBreaker {
    /// The consecutive failures that opened it.
    pub failures: u32,
    /// Those failures' classes, newest first (`untyped` for a failure that
    /// carried none, such as an owner's refused bind).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure_classes: Vec<String>,
    /// When the latest of them opened it.
    pub opened_at: DateTime<Utc>,
    /// When it half-opens to admit one probe claim.
    pub half_open_at: DateTime<Utc>,
}

impl PullBreaker {
    /// Whether it admits a probe claim at `now`.
    #[must_use]
    pub fn half_open(&self, now: DateTime<Utc>) -> bool {
        now >= self.half_open_at
    }

    /// `open after 3 consecutive failed claims (candidate, candidate,
    /// task_input) since 2026-10-11 16:53Z; half-opens at 2026-10-11 17:08Z to
    /// admit one probe claim`, or `half-open since ...` once that time passed.
    #[must_use]
    pub fn describe(&self, now: DateTime<Utc>) -> String {
        let classes = if self.failure_classes.is_empty() {
            String::new()
        } else {
            format!(" ({})", self.failure_classes.join(", "))
        };
        let half_open_at = self.half_open_at.format("%Y-%m-%d %H:%M:%SZ");
        let next = if self.half_open(now) {
            format!(
                "half-open since {half_open_at}: admits one probe claim once nothing this \
                 drain claimed is unsettled"
            )
        } else {
            format!("half-opens at {half_open_at} to admit one probe claim")
        };
        format!(
            "open after {} consecutive failed claims{classes} since {}; {next}",
            self.failures,
            self.opened_at.format("%Y-%m-%d %H:%M:%SZ"),
        )
    }
}
