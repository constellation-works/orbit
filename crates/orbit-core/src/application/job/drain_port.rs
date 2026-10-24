//! What job runs need from the distributed drain and pull settlement
//! [ORB-17937].
//!
//! Job submission takes the drain entry decision, crew draws skip crews
//! under a host provider hold, and cancel, stop and worker execution settle
//! the pull admissions a run carries. `distributed` and `pull_drain` own
//! those rules, and both also drive job runs, so a direct call from here
//! would close a cycle. These ports are the way in: `distributed` and
//! `pull_drain` implement them on [`OrbitRuntime`](crate::OrbitRuntime), so
//! composing the runtime binds them.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::TaskCommitBoundary;
use orbit_store::contracts::LocalPullAdmission;
use orbit_types::workflow::{JobRun, ResourceThrottle};

use crate::application::contracts::{
    DrainClaimedLeaf, DrainEntryPoint, HostProviderHold, PullSettlementEntry,
};

/// Drain admission and owner coordination reads job code calls,
/// implemented by `distributed`.
pub(in crate::application) trait JobDrainPort {
    /// Admit an operator's explicit `entry_point` invocation over
    /// `task_ids`, or refuse it the way the shared drain entry decision
    /// does.
    fn admit_explicit_drain_entry(
        &self,
        entry_point: DrainEntryPoint,
        task_ids: &[String],
    ) -> Result<(), OrbitError>;

    /// Refuse a drain on a host whose Git protection would fail every
    /// sandboxed leaf.
    fn drain_git_protection_preflight(&self) -> Result<(), OrbitError>;

    /// The host resource throttle admission currently sees, if any.
    fn admission_throttle(&self) -> Option<ResourceThrottle>;

    /// Unavailable and capacity holds still standing on this host at `now`.
    fn host_provider_holds(&self, now: DateTime<Utc>) -> Vec<HostProviderHold>;

    /// The claimed leaves pull drain `drain_run_id` is still carrying; empty
    /// for every other run.
    fn drain_claimed_leaves(&self, drain_run_id: &str)
    -> Result<Vec<DrainClaimedLeaf>, OrbitError>;

    /// The pull admissions drain `drain_run_id` carries that still hold a
    /// slot; empty for every other run.
    fn drain_carried_admissions(
        &self,
        drain_run_id: &str,
    ) -> Result<Vec<LocalPullAdmission>, OrbitError>;

    /// The completion authority this owner grants a distributed handoff, or
    /// `None` when each waits for an operator's approval.
    fn handoff_completion_authority(&self) -> Option<String>;

    /// The task commit boundary owner admissions are reserved through.
    fn owner_commit_boundary(&self) -> Result<TaskCommitBoundary, OrbitError>;
}

/// Pull settlement job run surfaces carry, implemented by `pull_drain`.
pub(in crate::application) trait JobPullSettlementPort {
    /// An operator's settle-only pass over every admission in this
    /// workspace that still holds a slot.
    fn settle_workspace_pulls(&self) -> Vec<PullSettlementEntry>;

    /// An operator's settle-only pass over `records`, releasing what never
    /// launched under `cause`.
    fn settle_carried_pulls(
        &self,
        records: Vec<LocalPullAdmission>,
        cause: &str,
    ) -> Vec<PullSettlementEntry>;

    /// Deliver claimed leaf `run_id`'s recorded settlement; `None` when the
    /// run is not a claimed leaf or its admission no longer holds a slot.
    fn deliver_leaf_settlement(&self, run_id: &str) -> Option<PullSettlementEntry>;

    /// Retry, from the leaf's own worker, a settlement claimed leaf `run_id`
    /// recorded but did not deliver.
    fn retry_leaf_settlement(&self, run_id: &str);

    /// Record the release of `record`'s claim for `leaf`, which an operator
    /// is stopping, with reason `why`. A settlement the leaf recorded first
    /// wins and is returned instead.
    fn record_stopped_leaf_release(
        &self,
        record: &LocalPullAdmission,
        leaf: &JobRun,
        why: &str,
    ) -> Result<LocalPullAdmission, OrbitError>;
}
