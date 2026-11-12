//! A follower's late report of a settled leaf's reasoning effort
//! [ORB-24115].

use chrono::Utc;
use orbit_common::OrbitError;

use super::super::TaskCommitBoundary;
use super::codec::{CLAIM, HANDOFF_SETTLEMENT, LATE_LEAF_EFFORT};
use super::{decode, invalid, row};
use crate::contracts::*;

impl TaskCommitBoundary {
    /// Record `effort` (`None` for the provider default) as the reasoning
    /// effort the settled claim `claim_id`'s leaf ran at, as `machine_id`
    /// reports it after the fact.
    ///
    /// The report is kept in its own immutable row and changes no claim, task,
    /// run or settlement record: inspection fills the settlement's effort from
    /// it only while the settlement holds none. It is refused unless the claim
    /// is settled, holds a settlement record, ran on `machine_id` and was bound
    /// to `run_id` there. A claim whose settlement or earlier report already
    /// holds the same effort replays; one holding a different effort keeps it
    /// and reports the conflict. Either way nothing is written.
    pub fn record_late_leaf_effort(
        &self,
        claim_id: &str,
        machine_id: &str,
        run_id: &str,
        effort: Option<String>,
    ) -> Result<LateLeafEffortWrite, OrbitError> {
        self.with_admission(|| {
            let claim: ExecutionClaim = self
                .store
                .task_coordination_row(&self.workspace_id, CLAIM, claim_id)?
                .map(|row| decode(&row.payload_json))
                .transpose()?
                .ok_or_else(|| invalid("unknown claim"))?;
            if claim.executed_on.machine_id != machine_id {
                return Err(OrbitError::PolicyDenied(
                    "not_settling_host: only the machine that ran and settled this claim's leaf \
                     may report its effort"
                        .into(),
                ));
            }
            if claim.phase.is_unsettled() {
                return Err(invalid(
                    "a late effort report is recorded only for a settled claim",
                ));
            }
            let state = self.claim_state(claim.clone())?;
            let bound = state
                .bound_run
                .as_ref()
                .is_some_and(|bound| bound.run_id == run_id && bound.machine_id == machine_id);
            if !bound {
                return Err(OrbitError::PolicyDenied(
                    "not_settling_host: the claim was not bound to this leaf run on its \
                     executing machine"
                        .into(),
                ));
            }
            let handoff = self
                .store
                .task_coordination_row(&self.workspace_id, HANDOFF_SETTLEMENT, claim_id)?
                .map(|row| decode::<ClaimSettlementRecord>(&row.payload_json))
                .transpose()?;
            let Some(settlement) = handoff.or(state.settlement) else {
                return Err(invalid(
                    "no_settlement_record: the claim settled before the owner recorded \
                     settlements, so there is no record to attribute an effort to",
                ));
            };
            if let Some(held) = settlement.reasoning_effort {
                return Ok(if held == effort {
                    LateLeafEffortWrite::Replayed
                } else {
                    LateLeafEffortWrite::Conflict { held }
                });
            }
            let report = LateLeafEffort {
                machine_id: machine_id.to_string(),
                run_id: run_id.to_string(),
                reasoning_effort: effort,
                recorded_at: Utc::now().to_rfc3339(),
            };
            let existing = self
                .store
                .task_coordination_row(&self.workspace_id, LATE_LEAF_EFFORT, claim_id)?
                .map(|row| decode::<LateLeafEffort>(&row.payload_json))
                .transpose()?;
            if let Some(existing) = existing {
                return Ok(if existing.same_fact(&report) {
                    LateLeafEffortWrite::Replayed
                } else {
                    LateLeafEffortWrite::Conflict {
                        held: existing.reasoning_effort,
                    }
                });
            }
            self.store.insert_task_coordination_row(
                &self.workspace_id,
                &row(LATE_LEAF_EFFORT, claim_id, &report)?,
                None,
            )?;
            Ok(LateLeafEffortWrite::Recorded)
        })
    }
}

/// Projection only: fill a settlement that holds no reported effort from the
/// claim's late report. A settlement that holds one keeps it; the store never
/// records a disagreeing report.
pub(super) fn attach_late_leaf_effort(
    state: &mut ClaimInspection,
    payload: &str,
) -> Result<(), OrbitError> {
    let report: LateLeafEffort = decode(payload)?;
    if let Some(settlement) = state.settlement.as_mut()
        && settlement.reasoning_effort.is_none()
    {
        settlement.reasoning_effort = Some(report.reasoning_effort);
    }
    Ok(())
}
