//! Operator-recorded leaf-run evidence for a settled claim [ORB-20906].

use chrono::Utc;
use orbit_common::OrbitError;

use super::super::TaskCommitBoundary;
use super::codec::{CLAIM, LEAF_RUN_EVIDENCE};
use super::{decode, invalid, row};
use crate::contracts::*;

impl TaskCommitBoundary {
    /// Record `report` as the leaf-run evidence of the settled claim
    /// `claim_id`, read from its executing host after the fact.
    ///
    /// The evidence row is separate and immutable. This changes no claim,
    /// task or run: the claim keeps its phase, state and settlement, and no
    /// history event is written. The claim must be settled and must have been
    /// bound to exactly `report.run_id` on its own executing machine. A claim
    /// that already holds the same fact replays it; one that holds a
    /// different fact is left untouched and reported as a conflict.
    pub fn record_hydrated_leaf_evidence(
        &self,
        claim_id: &str,
        report: LeafRunReport,
    ) -> Result<LeafEvidenceWrite, OrbitError> {
        self.with_admission(|| {
            let claim: ExecutionClaim = self
                .store
                .task_coordination_row(&self.workspace_id, CLAIM, claim_id)?
                .map(|row| decode(&row.payload_json))
                .transpose()?
                .ok_or_else(|| invalid("unknown claim"))?;
            if claim.phase.is_unsettled() {
                return Err(invalid(
                    "leaf run evidence is recorded only for a settled claim",
                ));
            }
            let state = self.claim_state(claim.clone())?;
            let bound_to_report = state.bound_run.as_ref().is_some_and(|bound| {
                bound.run_id == report.run_id && bound.machine_id == claim.executed_on.machine_id
            });
            if !bound_to_report {
                return Err(invalid(
                    "the claim was not bound to this leaf run on its executing machine",
                ));
            }
            if report.job_id.trim().is_empty() {
                return Err(invalid("leaf run evidence requires the leaf's job"));
            }
            let evidence = LeafRunEvidence {
                provenance: LeafEvidenceProvenance::Hydrated,
                recorded_at: Utc::now().to_rfc3339(),
                claim_id: claim.claim_id.clone(),
                task_id: claim.task_id.clone(),
                machine_id: claim.executed_on.machine_id.clone(),
                drain_run_id: claim.run_context.run_id.clone(),
                report: LeafRunReport {
                    root_cause: report
                        .root_cause
                        .map(LeafRootCause::bounded)
                        .filter(|cause| !cause.is_empty()),
                    ..report
                },
            };
            let existing = self
                .store
                .task_coordination_row(&self.workspace_id, LEAF_RUN_EVIDENCE, claim_id)?
                .map(|row| decode::<LeafRunEvidence>(&row.payload_json))
                .transpose()?;
            if let Some(existing) = existing {
                return Ok(if existing.same_fact(&evidence) {
                    LeafEvidenceWrite::Replayed(Box::new(existing))
                } else {
                    LeafEvidenceWrite::Conflict(Box::new(existing))
                });
            }
            self.store.insert_task_coordination_row(
                &self.workspace_id,
                &row(LEAF_RUN_EVIDENCE, claim_id, &evidence)?,
                None,
            )?;
            Ok(LeafEvidenceWrite::Recorded(Box::new(evidence)))
        })
    }
}
