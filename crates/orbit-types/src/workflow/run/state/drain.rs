//! Drain and crew policy state.

use std::collections::BTreeMap;

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

/// Why a follower cannot run a crew for the rest of its pull drain window.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CrewExclusionSource {
    /// The window's provider preflight: the crew is disabled, or its
    /// provider CLI cannot be found or resolved here.
    Preflight,
    /// A claimed leaf failed because the provider could not be used.
    /// Authentication excludes every configured crew of that provider; a
    /// capacity failure excludes only the leaf's crew.
    ProviderUnavailable,
    /// A claimed leaf on this crew was released for a failure class that
    /// [excludes its crew](ClaimFailureClass::excludes_crew) [ORB-14257].
    LeafReleased,
}

/// One crew a follower will not run, and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct CrewExclusion {
    pub crew: String,
    pub source: CrewExclusionSource,
    pub reason: String,
}

/// The crews a pull drain's window can run, from the provider preflight it
/// took when the window opened [ORB-13941]. Cached for the window: a fixed
/// credential or newly installed CLI takes effect with the next drain.
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

/// A graceful cancel of a pull drain that is waiting for its launched leaves.
///
/// The drain stops admitting at once and hands its unlaunched claims back to
/// the owner, but stays the same running coordinator until every leaf it
/// launched has finished and settled; only then does it end `cancelled`. A
/// forced cancel does not wait and records nothing here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainCancelRequest {
    pub actor: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub requested_at: DateTime<Utc>,
}

/// Task disposition chosen by an operator cancellation. Kept in the run's
/// durable state before the owner is signalled so whichever process wins run
/// finalization applies the same task outcome and preserves the cancel note.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskCancellationPolicy {
    /// `true` preserves the legacy `blocked` transition; `false` returns the
    /// task to `backlog` so its candidate can be resumed later.
    pub block: bool,
    /// Actor and optional reason captured for the task history entry.
    pub note: String,
}

/// A live operator adjustment to a bounded drain's worker ceiling [ORB-11253].
///
/// The ceiling a drain was submitted with lives in its immutable
/// `initial_input`, which is why raising it used to mean cancelling the
/// coordinator and submitting a replacement. This is the mutable counterpart:
/// the admission path prefers it over the submitted value, so the same run id,
/// deadline, completion policy, and already-dispatched children survive the
/// change.
///
/// `revision` is the compare-and-set handle. Every accepted update increments
/// it, so a caller that read revision *n* and writes with `expected_revision`
/// *n* cannot silently overwrite an update that landed in between.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainWorkerLimit {
    /// Live ceiling on concurrently live leaf runs.
    pub max_active_leaf_runs: u32,
    /// The ceiling this update replaced, kept as change evidence.
    pub previous_max_active_leaf_runs: u32,
    /// Accepted-update counter, starting at 1.
    pub revision: u32,
    pub actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// A backlog task a drain's admission pass left unstarted, and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainWaitingTask {
    pub task_id: String,
    /// Why the drain could not admit it (`context_lock_conflict`,
    /// `crew_not_allowed`, `host_os_mismatch`, ...); absent for a plain lock
    /// deferral.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The tasks holding what it needs, when known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    /// What clears the wait, when the reason alone does not say: the host OS
    /// a `host_os_mismatch` task waits for, say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What an `--approve-proposed` drain has done with `proposed` work.
///
/// The approval steps write it on the drain's own run state: the selection
/// step replaces the held view every pass, and the step after each task-pilot
/// child adds the tasks that child approved. Like `drain_last_pass` it
/// survives terminalization, so `run show` reports a finished drain's
/// approvals and holds.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DrainApprovalReport {
    pub recorded_at: Option<DateTime<Utc>>,
    /// Proposed tasks this drain moved to `backlog` over its whole window.
    #[serde(default)]
    pub approved_total: u64,
    /// The most recent of those (bounded list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved: Vec<String>,
    /// Proposed tasks the latest pass left proposed, with the reason: the
    /// qualification they miss, or the task-pilot classification that held
    /// them (bounded list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<DrainWaitingTask>,
    /// The full count behind `held`.
    #[serde(default)]
    pub held_total: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub held_by_reason: BTreeMap<String, u64>,
}

/// Workspace capacity observed before a local drain's latest admission wave.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DrainCapacity {
    /// Shared slots occupied by live deliveries and unsettled pull admissions.
    pub active_leaf_runs: u64,
    /// Occupied slots outside this coordinator's dispatch lineage or claims.
    pub inherited_leaf_runs: u64,
    /// Effective ceiling, including an operator's live resize.
    pub max_active_leaf_runs: u64,
}

/// What a drain's most recent admission pass left waiting.
///
/// The classifier's own output lives only inside the running loop, so this is
/// the one durable record of the backlog a finished drain never started. It is
/// overwritten every pass: the last one is the drain's final view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DrainAdmissionPass {
    /// Stable classifier of the latest failed pass, when available. A skew
    /// failure is terminal even when the drain window remains open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pass_error_code: Option<String>,
    pub recorded_at: DateTime<Utc>,
    /// Capacity at this pass, before it dispatches new work. Absent in older
    /// records and in pull passes, which use owner-side admission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<DrainCapacity>,
    /// Admissible tasks the pass did not admit: no free slot, or a lock
    /// conflict.
    pub queued: u64,
    /// The subset of `queued` a lock conflict kept out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred: Vec<DrainWaitingTask>,
    /// Backlog tasks the drain could not admit at all (bounded list).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<DrainWaitingTask>,
    /// The full count behind `excluded`.
    #[serde(default)]
    pub excluded_total: u64,
    /// Host resource pressure that held this pass's admissions [ORB-13901].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_throttle: Option<ResourceThrottle>,
    /// The latest failed pull pass; cleared by success before degradation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pass_error: Option<String>,
    /// Consecutive failed pull passes, independent of failed claimed leaves.
    #[serde(default)]
    pub consecutive_pass_failures: u32,
    /// Sticky warning state: new admissions stop, but settlements keep flowing.
    /// Start a new drain after fixing the cause.
    #[serde(default)]
    pub degraded: bool,
}

/// One host resource whose sustained pressure holds new admissions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourcePressure {
    /// `cpu`, `memory`, or `disk <path>`.
    pub resource: String,
    pub percent: f64,
    pub high_percent: u8,
    pub resume_percent: u8,
    /// When the reading crossed its high mark.
    pub since: DateTime<Utc>,
}

/// Sustained host resource pressure holding new admissions [ORB-13901].
///
/// A throttle starts no new task; running work is never cancelled, paused or
/// killed. Admission resumes once every listed resource is back below its
/// resume mark.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceThrottle {
    pub resources: Vec<ResourcePressure>,
}

impl ResourceThrottle {
    /// `memory 93% (throttled at ≥ 90% since 2026-10-04 08:41Z; resumes below 80%); cpu 97% (throttled at ≥ 90% since 2026-10-04 08:40Z; resumes below 75%)`.
    #[must_use]
    pub fn describe(&self) -> String {
        self.resources
            .iter()
            .map(|pressure| {
                format!(
                    "{} {:.0}% (throttled at \u{2265} {}% since {}; resumes below {}%)",
                    pressure.resource,
                    pressure.percent,
                    pressure.high_percent,
                    pressure.since.format("%Y-%m-%d %H:%MZ"),
                    pressure.resume_percent,
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// The operator-facing hold sentence every surface prints.
    #[must_use]
    pub fn hold_reason(&self) -> String {
        format!(
            "Admissions throttled: {}. Running work is not touched.",
            self.describe()
        )
    }
}
