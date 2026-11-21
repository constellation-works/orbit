//! The distributed drain's side of the job-owned port [ORB-17937].

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::TaskCommitBoundary;
use orbit_store::contracts::{AdmissionReceipt, DrainLeafOccupancy, LocalPullAdmission};
use orbit_types::workflow::ResourceThrottle;

use crate::OrbitRuntime;
use crate::application::contracts::{
    ClaimedLeafStage, DrainClaimedLeaf, DrainEntryPoint, HostProviderHold, PullCrewWindow,
    PullLeafClaim, RefusedPullSettlement,
};
use crate::application::job::JobDrainPort;

impl JobDrainPort for OrbitRuntime {
    fn admit_explicit_drain_entry(
        &self,
        entry_point: DrainEntryPoint,
        task_ids: &[String],
    ) -> Result<(), OrbitError> {
        self.drain_entry_admission(entry_point, task_ids)?
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

    fn run_pull_claim(&self, run_id: &str) -> Result<Option<PullLeafClaim>, OrbitError> {
        self.pull_leaf_claim(run_id)
    }

    fn drain_refused_settlements(
        &self,
        run_id: &str,
    ) -> Result<Vec<RefusedPullSettlement>, OrbitError> {
        self.pull_drain_refused_settlements(run_id)
    }

    fn drain_crew_window(&self, run_id: &str) -> Result<Option<PullCrewWindow>, OrbitError> {
        self.pull_drain_crew_window(run_id)
    }

    fn drain_pull_occupancy(&self, run_id: &str) -> Result<Option<DrainLeafOccupancy>, OrbitError> {
        self.pull_drain_occupancy(run_id)
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

    fn claim_admission_receipt(&self, claim_id: &str) -> Result<AdmissionReceipt, OrbitError> {
        self.claim_receipt(claim_id)
    }

    fn held_claimed_leaf_stage(
        &self,
        task_id: &str,
    ) -> Result<Option<(ClaimedLeafStage, String, String)>, OrbitError> {
        self.claimed_leaf_stage_for_task(task_id)
    }

    fn drain_pull_gate_drift(&self, drain_run_id: &str) -> Result<Option<String>, OrbitError> {
        self.pull_gate_drift(drain_run_id)
    }
}
