use chrono::Utc;
use orbit_common::OrbitError;

use super::super::{Section, TaskCommitBoundary};
use super::codec::{CLAIM, HANDOFF_SETTLEMENT, STATE};
use super::{decode, decode_state, invalid};
use crate::contracts::*;

impl TaskCommitBoundary {
    /// Strictly read-only: an interrupted commit requires explicit journal recovery
    /// or an ordinary operational read first. Inspection never performs that repair.
    pub fn inspect_execution_claims(&self) -> Result<Vec<ClaimInspection>, OrbitError> {
        let section = Section::here("inspection");
        self.shared(&self.host_lock_target(), section, || {
            self.shared(&self.lock_target(), section, || {
                if self.interrupted_commit()? {
                    return Err(invalid(
                        "claim inspection unavailable until pending commit is recovered",
                    ));
                }
                self.claim_states_locked()
            })
        })
    }

    /// [ORB-12575] The ordinary-participant counterpart of
    /// [`Self::inspect_execution_claims`]: the same claim states, read inside
    /// the boundary so an interrupted commit is replayed first exactly as every
    /// other runtime read does. Live commands that merely consult claims (job
    /// resume) take this route; `orbit doctor` keeps the non-repairing read.
    pub fn resolve_execution_claims(&self) -> Result<Vec<ClaimInspection>, OrbitError> {
        self.enter_ordinary(|| self.claim_states_locked())
    }

    /// [ORB-15962] One claim's state, read inside the boundary like
    /// [`Self::resolve_execution_claims`]. Claim and lifecycle rows outlive
    /// the mutation receipts that settlement prunes.
    pub fn execution_claim_state(
        &self,
        claim_id: &str,
    ) -> Result<Option<ClaimInspection>, OrbitError> {
        self.enter_ordinary(|| {
            self.store
                .task_coordination_row(&self.workspace_id, CLAIM, claim_id)?
                .map(|row| {
                    let mut state = self.claim_state(decode(&row.payload_json)?)?;
                    if let Some(attribution) = self.store.task_coordination_row(
                        &self.workspace_id,
                        HANDOFF_SETTLEMENT,
                        claim_id,
                    )? {
                        attach_handoff_settlement(&mut state, &attribution.payload_json)?;
                    }
                    Ok(state)
                })
                .transpose()
        })
    }

    /// Caller holds the boundary and has already settled or excluded a
    /// pending commit.
    pub(in super::super) fn claim_states_locked(&self) -> Result<Vec<ClaimInspection>, OrbitError> {
        // Read the immutable sidecars once, rather than one query per claim.
        let attribution: std::collections::BTreeMap<_, _> = self
            .store
            .task_coordination_rows(&self.workspace_id, HANDOFF_SETTLEMENT)?
            .into_iter()
            .map(|row| (row.row_id, row.payload_json))
            .collect();
        self.store
            .task_coordination_rows_with_related_payload(&self.workspace_id, CLAIM, STATE)?
            .into_iter()
            .map(|(row, state)| {
                let claim = decode(&row.payload_json)?;
                let mut state = claim_state_from_payload(claim, state.as_deref())?;
                if let Some(payload) = attribution.get(&state.claim.claim_id) {
                    attach_handoff_settlement(&mut state, payload)?;
                }
                Ok(state)
            })
            .collect()
    }

    pub(super) fn claim_state(&self, claim: ExecutionClaim) -> Result<ClaimInspection, OrbitError> {
        let existing =
            self.store
                .task_coordination_row(&self.workspace_id, STATE, &claim.claim_id)?;
        claim_state_from_payload(
            claim,
            existing.as_ref().map(|row| row.payload_json.as_str()),
        )
    }
}

/// Projection only: mutations keep reading/writing the compatible lifecycle
/// row, and neither old nor new lifecycle rewrites can discard attribution.
fn attach_handoff_settlement(state: &mut ClaimInspection, payload: &str) -> Result<(), OrbitError> {
    let settlement: ClaimSettlementRecord = decode(payload)?;
    if settlement.kind != ClaimSettlementKind::Handoff
        || state
            .settlement
            .as_ref()
            .is_some_and(|existing| existing != &settlement)
    {
        return Err(invalid("inconsistent handoff settlement attribution"));
    }
    state.settlement = Some(settlement);
    Ok(())
}

/// Both the batched inspection and a single-claim read reconstruct legacy
/// claims without a lifecycle row and refresh the authoritative claim payload.
fn claim_state_from_payload(
    claim: ExecutionClaim,
    payload: Option<&str>,
) -> Result<ClaimInspection, OrbitError> {
    let mut state = match payload {
        Some(payload) => decode_state(payload)?,
        None => {
            let created = chrono::DateTime::parse_from_rfc3339(&claim.reservation_expires_at)
                .map_err(|e| OrbitError::Store(e.to_string()))?
                - chrono::Duration::seconds(
                    super::super::admission::ADMISSION_RESERVATION_TTL_SECONDS.into(),
                );
            ClaimInspection {
                claim: claim.clone(),
                bound_run: None,
                created_at: created.to_rfc3339(),
                updated_at: created.to_rfc3339(),
                last_event: "claimed".into(),
                age_seconds: None,
                unresolved_merge_intent: None,
                landing_invalidated: false,
                release: None,
                preserved_candidate: None,
                settlement: None,
                steering: None,
            }
        }
    };
    state.claim = claim;
    state.age_seconds = chrono::DateTime::parse_from_rfc3339(&state.created_at)
        .ok()
        .map(|created| {
            (Utc::now() - created.with_timezone(&Utc))
                .num_seconds()
                .max(0)
        });
    Ok(state)
}
