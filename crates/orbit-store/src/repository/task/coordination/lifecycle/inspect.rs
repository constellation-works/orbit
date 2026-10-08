use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::io::with_shared_file_lock;

use super::super::{COORDINATION_LOCK_LABEL, TaskCommitBoundary};
use super::codec::{CLAIM, STATE};
use super::{decode, invalid};
use crate::contracts::*;

impl TaskCommitBoundary {
    /// Strictly read-only: an interrupted commit requires explicit journal recovery
    /// or an ordinary operational read first. Inspection never performs that repair.
    pub fn inspect_execution_claims(&self) -> Result<Vec<ClaimInspection>, OrbitError> {
        with_shared_file_lock(&self.host_lock_target(), COORDINATION_LOCK_LABEL, || {
            with_shared_file_lock(&self.lock_target(), COORDINATION_LOCK_LABEL, || {
                if self.pending_marker_path().try_exists()? {
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

    /// Caller holds the boundary and has already settled or excluded a
    /// pending commit.
    pub(in super::super) fn claim_states_locked(&self) -> Result<Vec<ClaimInspection>, OrbitError> {
        self.store
            .task_coordination_rows(&self.workspace_id, CLAIM)?
            .iter()
            .map(|r| {
                let claim = decode(&r.payload_json)?;
                self.claim_state(claim)
            })
            .collect()
    }

    pub(super) fn claim_state(&self, claim: ExecutionClaim) -> Result<ClaimInspection, OrbitError> {
        let existing =
            self.store
                .task_coordination_row(&self.workspace_id, STATE, &claim.claim_id)?;
        let mut state = match existing {
            Some(row) => decode::<ClaimInspection>(&row.payload_json)?,
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
}
