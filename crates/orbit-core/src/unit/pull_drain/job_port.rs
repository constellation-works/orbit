//! Pull settlement's side of the job-owned port [ORB-17937].

use orbit_common::OrbitError;
use orbit_store::contract::LocalPullAdmission;
use orbit_types::workflow::JobRun;

use super::settle;
use crate::OrbitRuntime;
use crate::application::contracts::PullSettlementEntry;
use crate::application::job::JobPullSettlementPort;

pub(crate) struct JobPort<'a>(pub(crate) &'a OrbitRuntime);

impl JobPullSettlementPort for JobPort<'_> {
    fn settle_orphaned_admissions(&self) -> Vec<PullSettlementEntry> {
        settle::settle_orphaned_pulls(self.0)
    }

    fn settle_workspace_pulls(&self) -> Vec<PullSettlementEntry> {
        settle::settle_pending_pulls(self.0)
    }

    fn settle_carried_pulls(
        &self,
        records: Vec<LocalPullAdmission>,
        cause: &str,
    ) -> Vec<PullSettlementEntry> {
        settle::carry_settlements_for(self.0, records, cause)
    }

    fn deliver_leaf_settlement(&self, run_id: &str) -> Option<PullSettlementEntry> {
        settle::deliver_claimed_leaf_settlement(self.0, run_id)
    }

    fn settle_terminalized_leaf(&self, run_id: &str, diagnostic: Option<(&str, &str)>) {
        settle::best_effort_settle_terminal_claimed_leaf(self.0, run_id, diagnostic);
    }

    fn retry_leaf_settlement(&self, run_id: &str) {
        settle::retry_own_claimed_leaf_settlement(self.0, run_id);
    }

    fn record_stopped_leaf_release(
        &self,
        record: &LocalPullAdmission,
        leaf: &JobRun,
        why: &str,
    ) -> Result<LocalPullAdmission, OrbitError> {
        settle::record_forced_leaf_release(self.0, record, leaf, why)
    }
}
