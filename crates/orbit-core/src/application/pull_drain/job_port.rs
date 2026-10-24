//! Pull settlement's side of the job-owned port [ORB-17937].

use orbit_common::OrbitError;
use orbit_store::contracts::LocalPullAdmission;
use orbit_types::workflow::JobRun;

use crate::OrbitRuntime;
use crate::application::contracts::PullSettlementEntry;
use crate::application::job::JobPullSettlementPort;

impl JobPullSettlementPort for OrbitRuntime {
    fn settle_workspace_pulls(&self) -> Vec<PullSettlementEntry> {
        self.settle_pending_pulls()
    }

    fn settle_carried_pulls(
        &self,
        records: Vec<LocalPullAdmission>,
        cause: &str,
    ) -> Vec<PullSettlementEntry> {
        self.carry_settlements_for(records, cause)
    }

    fn deliver_leaf_settlement(&self, run_id: &str) -> Option<PullSettlementEntry> {
        self.deliver_claimed_leaf_settlement(run_id)
    }

    fn retry_leaf_settlement(&self, run_id: &str) {
        self.retry_own_claimed_leaf_settlement(run_id);
    }

    fn record_stopped_leaf_release(
        &self,
        record: &LocalPullAdmission,
        leaf: &JobRun,
        why: &str,
    ) -> Result<LocalPullAdmission, OrbitError> {
        self.record_forced_leaf_release(record, leaf, why)
    }
}
