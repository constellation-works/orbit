use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_types::task::{Task, TaskReferenceIndex, TaskStatus};
use serde::Serialize;

use super::super::unknown_footprint::FootprintWait;
use crate::runtime::task::locks::TaskLockOverlap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(in crate::adapter::engine_host::actions) struct BacklogTaskExclusion {
    pub(in crate::adapter::engine_host::actions) id: String,
    pub(in crate::adapter::engine_host::actions) reason: BacklogTaskExclusionReason,
    pub(in crate::adapter::engine_host::actions) conflicts: Vec<BacklogTaskConflict>,
    /// The crew the task would have dispatched as, on a
    /// [`BacklogTaskExclusionReason::CrewNotAllowed`] exclusion. Naming the
    /// *effective* crew — not the raw `task.crew`, which is often unset and
    /// inherited — is what makes the exclusion actionable [ORB-11242].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::adapter::engine_host::actions) crew: Option<String>,
    /// What an operator has to change for this task to become admissible, on
    /// an exclusion the drain cannot resolve by itself. A lock conflict clears
    /// when the holder finishes and needs no instruction; an
    /// [`BacklogTaskExclusionReason::InheritedOnlyEpicRoot`] never clears until
    /// the task is edited, so it carries one. A lock conflict whose task
    /// reserves its surface says so here, and the
    /// [`BacklogTaskExclusionReason::SurfaceReserved`] exclusions it causes
    /// name it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::adapter::engine_host::actions) detail: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::adapter::engine_host::actions) enum BacklogTaskExclusionReason {
    ContextLockConflict,
    /// The task selects a delivery job with a `delivery:<job>` tag that cannot
    /// deliver it in the drain's ship mode — its plugin is disabled or
    /// uninstalled, or the job does not declare that mode. The task stays in
    /// `backlog` rather than failing a gate run on every drain pass; `detail`
    /// carries the refusal, naming the plugin when one is installed.
    DeliveryJobUnavailable,
    /// The drain delivers in PR mode, the task would ship through the PR
    /// pipeline, and no Git remote of the checkout names a network host, so
    /// `pr_open` could never succeed. `detail` names the remotes,
    /// `orbit workspace ship-mode local`, and the `delivery:task_local_pipeline`
    /// tag that delivers just this task locally.
    PrForgeRemoteMissing,
    /// The run window permits a set of crews and this task's effective crew is
    /// not one of them [ORB-11242]. The task is left in `backlog` exactly as
    /// it is — never silently re-crewed — and the remaining eligible work
    /// keeps filling the drain's slots.
    CrewNotAllowed,
    /// The task's `os:` tags name no OS this host runs. It stays in `backlog`
    /// for a host that does — a follower pulling from this owner, say — and
    /// `detail` names the wait (`waits for a macos host (os:macos)`).
    HostOsMismatch,
    GroupMemberConflict,
    /// An `epic`-tagged root that declared no `context_files` of its own while
    /// its descendants did. Admission no longer inherits their surface, so the
    /// root would take a slot while reserving nothing — the population is
    /// [`orbit_types::task::inherited_only_epic_roots`]. Only an edit to the
    /// task clears this, so the exclusion carries a `detail`.
    InheritedOnlyEpicRoot,
    /// Automated work must be prepared before an implementation lane can
    /// consume it; urgency does not substitute for a complexity assessment.
    /// Work tagged [`orbit_types::task::NO_DIFF_EXPECTED_TAG`] is exempt — see
    /// [`super::exclusions::clears_complexity_gate`].
    UnassessedComplexity,
    /// A live task-pilot run holds a successful preparation checkpoint for
    /// this task. Delivery waits until that run settles its assessment.
    ActivePilotPreparation,
    /// The latest applied pilot assessment identifies duplicate work. Only a
    /// newer clear assessment or an explicit operator decision releases it.
    PilotDuplicate,
    /// The latest applied pilot assessment identifies work already landed.
    PilotAlreadyLanded,
    /// Current validation needs an operator-reserved governed operation.
    OperatorValidationHandoff,
    /// The latest assessment identifies operator-side work no managed lane can perform.
    HostOperationalHandoff,
    /// The latest applied pilot assessment found a criterion that needs
    /// native evidence from an OS this host does not run, and the task's
    /// `os:` tags do not name that OS. `detail` names the criterion and the
    /// tag to add; the tag, a re-scope, an operator decision or a newer
    /// assessment without the finding clears it. A host of that OS may
    /// still start the task.
    NativeOsRequired,
    /// The task is a review-filed bug without a CONFIRMED verdict from the
    /// finding verification job. `detail` names the job to run; a CONFIRMED
    /// verdict, or a retag or retype by an operator, clears it.
    UnverifiedReviewFinding,
    /// Effective `review.before_pr` is on and this delivery is the local-only
    /// route. Pipeline admission refuses that combination; the task stays in
    /// `backlog` until the switch is turned off or delivery uses the PR route.
    /// `detail` names the deciding config layer and the remedy [ORB-14168].
    LocalRouteBeforePr,
    /// Effective `review.before_landing` is on and this delivery is the
    /// local-only route, which opens no pull request to review. Pipeline
    /// admission refuses it the same way; `detail` names the deciding config
    /// layer and the remedy [ORB-14849].
    LocalRouteBeforeLanding,
    /// The task's last delivery failed a required command its base fails the
    /// same way, and the base has not moved to a commit that may pass it
    /// [ORB-14258]. The task stays in `backlog`; the hold lifts by itself once
    /// the command passes on a new base tip, and `detail` names the base and
    /// command.
    BaselineRedHold,
    /// The task's last local run failed on its provider (capacity, an
    /// unusable provider, or a content-policy refusal), and the hold that
    /// failure placed excludes every crew the task could run as until its
    /// `not_before` [ORB-14266]. A hold that leaves another crew admits the
    /// task on that crew instead. `detail` names the run, the failure, the
    /// excluded crews and the time.
    ProviderBackoff,
    /// Every crew the task may run as belongs to a provider whose latest usage
    /// reading on this host is exhausted, or at or above the configured
    /// threshold, and the reading is still live [ORB-14697]. A crew left
    /// unlimited takes the task instead. The exclusion lifts by itself once
    /// the reading's reset passes; `detail` names the provider, window, used
    /// percent, threshold, reset and the crews skipped.
    ProviderLimit,
    /// A critical or high-priority task ranked ahead of this one waits only on
    /// context locks, and this task's surface overlaps the surface it reserves.
    /// Admitting this task would take a lock the reserving task needs as soon
    /// as it frees, so it waits until the reserving task is admitted or leaves
    /// `backlog`. `conflicts` names the reserving task; reservations are
    /// bounded per pass (see `surface_reservation`).
    SurfaceReserved,
    /// The task declares no footprint and a task pilot on this host will
    /// prepare one; a multi-slot ship waits for it [ORB-15191]. `detail`
    /// names the deadline and the fix.
    AwaitingFootprint,
    /// The task declares no footprint, no pilot will prepare one, and other
    /// work is in flight or ranked ahead, so a multi-slot ship leaves it for a
    /// run where it goes alone [ORB-15191]. `detail` names the fix.
    AwaitingExclusiveSlot,
}

/// The overlap `orbit task eligible` reports, so a conflict means the same
/// thing to a drain and to an operator choosing work.
pub(in crate::adapter::engine_host::actions) type BacklogTaskConflict = TaskLockOverlap;

/// The task population and leaf eligibility result shared by automatic
/// dispatch and its read-only diagnostic.  Keeping the lock filter here
/// prevents the diagnostic from becoming a second scheduler.
pub(in crate::adapter::engine_host::actions) struct BacklogSnapshot {
    /// Every open task in the workspace and the closed tasks the snapshot's
    /// tree rules reach — ancestors for the group-conflict roll-up,
    /// descendants of an epic root that declares no context — plus whatever
    /// [`BacklogSnapshot::include_tasks`] added, as envelope metadata (see
    /// `population::admission_population`): the body documents
    /// (`description`, `acceptance_criteria`, `plan`, `execution_summary`) are
    /// empty, and nothing in admission, exclusion or readiness reads them.
    /// This is the one materialized copy; the other fields refer into it by ID
    /// rather than holding clones.
    pub(in crate::adapter::engine_host::actions) task_lookup: BTreeMap<String, Task>,
    /// The registry-global status projection. Deliberately not derived from
    /// `task_lookup`: task lists are workspace-scoped, dependency readiness
    /// is not, and a dependency on a task in another workspace resolves only
    /// here. Backlog tasks' archived dependencies that reached `done` first
    /// project as `done` (`OrbitRuntime::dependency_status_index`).
    pub(in crate::adapter::engine_host::actions) status_by_id: BTreeMap<String, TaskStatus>,
    pub(in crate::adapter::engine_host::actions) reference_index: TaskReferenceIndex,
    /// Admissible leaf task IDs in dispatch order; the tasks are in
    /// `task_lookup`.
    pub(in crate::adapter::engine_host::actions) admissible_leaves: Vec<String>,
    pub(in crate::adapter::engine_host::actions) excluded: Vec<BacklogTaskExclusion>,
    /// Selector -> the `in-progress` / `review` tasks holding it. Carried on
    /// the snapshot rather than recomputed by each consumer so admission
    /// selection, exclusion reasons, and the lock-wait diagnostic all name the
    /// same holder for the same selector [ORB-11973].
    pub(in crate::adapter::engine_host::actions) lock_holders: BTreeMap<String, Vec<String>>,
    /// Backlog tasks minted over a frozen delivery batch that nears its
    /// admission deadline, with that deadline [ORB-14624]. They sort ahead of
    /// same-priority backlog in `admissible_leaves`.
    pub(in crate::adapter::engine_host::actions) expiring_batches: BTreeMap<String, DateTime<Utc>>,
    /// The admissible leaves that declare no footprint, and what each waits
    /// for in a multi-slot run [ORB-15191]. The snapshot does not know the
    /// run's slots, so the wave applies them, not this filter.
    pub(in crate::adapter::engine_host::actions) footprint_waits: BTreeMap<String, FootprintWait>,
}
