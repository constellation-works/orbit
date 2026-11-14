//! Executor census: the rows recording which machines can run admitted work.

use std::collections::BTreeSet;

use orbit_common::OrbitError;

use super::super::TaskCommitBoundary;
use crate::contracts::*;

impl TaskCommitBoundary {
    /// Upsert `entry` as its machine's census row [ORB-17304].
    pub fn record_executor_census(&self, entry: &ExecutorCensusEntry) -> Result<(), OrbitError> {
        self.store.upsert_executor_census(&self.workspace_id, entry)
    }

    /// Every census row of this workspace, however old.
    pub fn recorded_executor_census(&self) -> Result<Vec<ExecutorCensusEntry>, OrbitError> {
        self.store.executor_census_rows(&self.workspace_id)
    }

    /// The census rows live at `now`: each seen within
    /// [`EXECUTOR_CENSUS_LIVENESS_SECONDS`], or of a machine still holding a
    /// claimed or running claim. Claims are read only when a row has gone
    /// stale.
    pub fn live_executor_census(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<ExecutorCensusEntry>, OrbitError> {
        let (mut live, stale): (Vec<_>, Vec<_>) = self
            .store
            .executor_census_rows(&self.workspace_id)?
            .into_iter()
            .partition(|entry| entry.fresh(now));
        if !stale.is_empty() {
            let claiming = self
                .execution_claims()?
                .into_iter()
                .filter(|claim| {
                    matches!(
                        claim.phase,
                        ExecutionClaimPhase::Claimed | ExecutionClaimPhase::Running
                    )
                })
                .map(|claim| claim.executed_on.machine_id)
                .collect::<BTreeSet<_>>();
            live.extend(
                stale
                    .into_iter()
                    .filter(|entry| claiming.contains(&entry.machine_id)),
            );
        }
        Ok(live)
    }
}
