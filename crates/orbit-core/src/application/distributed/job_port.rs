//! The distributed drain's side of the job-owned port [ORB-17937].

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::TaskCommitBoundary;
use orbit_store::contracts::LocalPullAdmission;
use orbit_types::workflow::ResourceThrottle;

use crate::OrbitRuntime;
use crate::application::contracts::{DrainClaimedLeaf, DrainEntryPoint, HostProviderHold};
use crate::application::job::JobDrainPort;

impl JobDrainPort for OrbitRuntime {
    fn admit_explicit_drain_entry(
        &self,
        entry_point: DrainEntryPoint,
        task_ids: &[String],
    ) -> Result<(), OrbitError> {
        self.drain_entry_admission(entry_point, task_ids, false)?
            .into_result()
            .map(|_| ())
    }

    fn drain_git_protection_preflight(&self) -> Result<(), OrbitError> {
        self.preflight_git_protection()
    }

    fn admission_throttle(&self) -> Option<ResourceThrottle> {
        self.admission_resource_throttle().throttle
    }

    fn host_provider_holds(&self, now: DateTime<Utc>) -> Vec<HostProviderHold> {
        self.standing_host_provider_holds(now)
    }

    fn drain_claimed_leaves(
        &self,
        drain_run_id: &str,
    ) -> Result<Vec<DrainClaimedLeaf>, OrbitError> {
        self.pull_drain_claimed_leaves(drain_run_id)
    }

    fn drain_carried_admissions(
        &self,
        drain_run_id: &str,
    ) -> Result<Vec<LocalPullAdmission>, OrbitError> {
        self.pull_drain_admissions(drain_run_id)
    }

    fn handoff_completion_authority(&self) -> Option<String> {
        self.owner_completion_authority()
    }

    fn owner_commit_boundary(&self) -> Result<TaskCommitBoundary, OrbitError> {
        self.admission_boundary()
    }
}
