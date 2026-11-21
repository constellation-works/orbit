//! Pull-drain run-detail values shared by job projections and distributed reads.

use chrono::{DateTime, Utc};
use orbit_store::contracts::AdmissionCrewPressure;
use orbit_types::workflow::{ClaimFailureClass, CrewExclusion, PullAuthExclusion};
use serde::Serialize;

/// The crews a pull drain's window can run, and those it will not [ORB-13941].
///
/// The window's provider preflight, taken once when it opened, minus every
/// crew a claimed leaf of this drain found unusable since — an authentication
/// failure excludes every configured crew of that provider account for the
/// window (a crew that sets `env` is an account of its own [ORB-17288]), a
/// capacity failure only the leaf's crew and only for a bounded backoff
/// [ORB-15791] — and, when the drain was submitted with `--allow-crew`,
/// outside that restriction [ORB-14174]. Derived from the drain's run input
/// and its own admission records, so it survives a follower restart and a
/// resume, and ends with the drain.
///
/// [ORB-20400] Every name match counts a crew's family: an exclusion of
/// `opus` keeps `opus@medium` off the window too, and a capacity backoff of
/// `opus@medium` backs off `opus` and its other effort references, which
/// share the model and account.
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
    /// Whether `host_suppressed` came from repeated busy-owner episodes
    /// rather than a failure of this host: the remedy is on the owner.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub host_suppressed_by_busy_owner: bool,
    /// While set, this drain requests no claim until `until`: a claimed leaf
    /// was released because the owner's store stayed locked, which blames
    /// neither this host nor its crew. It lifts by itself in the same window;
    /// repeated episodes suppress the host instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_busy_backoff: Option<OwnerBusyBackoff>,
    /// Crews the window can run once its timed exclusions lift, before
    /// `allowed` narrows them. The owner's reviewer runs as one of these:
    /// the restriction selects the implementation crews a claim may carry,
    /// not the review it owes.
    #[serde(skip)]
    pub(in crate::application) reviewable: Option<Vec<String>>,
    /// Preflight crews permitted by the effective allowlist, before timed holds.
    #[serde(skip)]
    pub(in crate::application) permitted: Option<Vec<String>>,
    /// Per-crew weekly usage pressure from this host's own readings
    /// [ORB-17773], recomputed each pass like the limit gate. Empty under
    /// `workflow.crew_selection = "static"`: then the capability reports
    /// none and the owner orders this follower's offers as before.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub usage_pressure: Vec<AdmissionCrewPressure>,
}

/// A drain's bounded backoff from an owner whose store stayed locked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OwnerBusyBackoff {
    /// When requests resume by themselves.
    pub until: DateTime<Utc>,
    /// Busy-owner episodes in this window so far, this one included.
    pub episodes: u32,
    /// Releases for the owner's busy store in this window so far.
    pub releases: u32,
    /// The latest such release.
    pub reason: String,
}

/// A settlement the owner refused while it still holds the claim
/// [ORB-13979]: the record stays `settling`, its outcome kept, and delivery
/// backs off rather than asking again on every pass. `orbit run show` lists
/// these for the drain carrying them. A pass admits nothing new while a
/// non-delivery refusal is held for its owner; a delivery-evidence refusal
/// holds only its own settlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefusedPullSettlement {
    /// The owner's task the settlement is for.
    pub task_id: Option<String>,
    pub leaf_run_id: Option<String>,
    /// The owner's host-qualified selector.
    pub owner: String,
    /// The drain run that admitted the claim.
    pub drain_run_id: String,
    /// The owner's refusal, as it answered.
    pub reason: String,
    /// Stable machine-readable refusal code, when the owner answered with one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// Consecutive refused deliveries.
    pub refusals: u32,
    pub first_refused_at: DateTime<Utc>,
    pub last_refused_at: DateTime<Utc>,
    /// No automatic pass delivers it before this.
    pub retry_after: DateTime<Utc>,
    /// What the operator does about it.
    pub remedy: String,
}

/// The owner claim a local pull leaf run executes, as this follower recorded
/// it. `orbit run show <leaf-run>` reads this so a leaf's run page names the
/// task it works for, which owner holds the claim, and whether the outcome has
/// reached that owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PullLeafClaim {
    /// The owner's task the leaf executes.
    pub task_id: Option<String>,
    /// The owner's claim on that task.
    pub claim_id: Option<String>,
    /// The owner's host-qualified selector.
    pub owner: String,
    /// The drain run that admitted the claim.
    pub drain_run_id: String,
    /// Where the follower's record of this admission stands (`launched`,
    /// `settling`, `settled`, ...).
    pub settlement_phase: String,
    /// Why the owner refused the settlement, once it had already ended the
    /// claim.
    pub refusal: Option<String>,
    /// The owner's refusal of the pending settlement while it still holds
    /// the claim, and when delivery is next attempted.
    pub settlement_refusal: Option<RefusedPullSettlement>,
    /// [ORB-14257] Why the leaf ended without its handoff, as its recorded
    /// settlement types it; `None` until a failure or release is recorded,
    /// and for a release of a leaf that never launched.
    pub failure_class: Option<ClaimFailureClass>,
    /// What the phase means for the operator.
    pub guidance: String,
}
