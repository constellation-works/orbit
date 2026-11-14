//! Mid-run steering's records for claimed leaves, as their owner keeps them
//! on each claim's lifecycle state (`docs/design/mid-run-steering/2_design.md`
//! §3).

use orbit_common::{ClaimRefusalKind, OrbitError};
use orbit_types::workflow::SteeringWatermark;

use super::super::TaskCommitBoundary;
use super::{STATE, decode_state};
use crate::contracts::*;

impl TaskCommitBoundary {
    /// The steering records a claimed leaf's promotion gate and prompts read
    /// from the owner: its own claim's watermark, and the watermark the
    /// task's most recent other claim left. Refused as `stale_claim` exactly
    /// as the leaf's steering write would be, so a leaf whose claim the owner
    /// no longer runs certifies nothing.
    pub fn worker_claim_steering(
        &self,
        auth: &ClaimInvocation,
    ) -> Result<ClaimSteeringReading, OrbitError> {
        if auth.operator {
            return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
        }
        self.with_admission(|| {
            let authority = self.claim_authority(auth, false)?;
            if authority.claim.phase != ExecutionClaimPhase::Running {
                return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
            }
            Ok(ClaimSteeringReading {
                claim: authority.state.steering,
                inherited: self.latest_claim_steering(&auth.task_id, Some(&auth.claim_id))?,
            })
        })
    }

    /// The steering watermark the task's most recent claim recorded, kept
    /// through that claim's settlement: the baseline for an owner run that
    /// carries the claim's candidate and built no prompt of its own.
    pub fn task_claim_steering(
        &self,
        task_id: &str,
    ) -> Result<Option<SteeringWatermark>, OrbitError> {
        self.latest_claim_steering(task_id, None)
    }

    /// The watermark of `task_id`'s most recently updated claim that
    /// recorded one, other than `except`.
    fn latest_claim_steering(
        &self,
        task_id: &str,
        except: Option<&str>,
    ) -> Result<Option<SteeringWatermark>, OrbitError> {
        let mut latest: Option<(chrono::DateTime<chrono::FixedOffset>, SteeringWatermark)> = None;
        for row in self.coordination_rows_mentioning(STATE, task_id)? {
            let state = decode_state(&row.payload_json)?;
            if state.claim.task_id != task_id || except == Some(state.claim.claim_id.as_str()) {
                continue;
            }
            let Some(watermark) = state.steering else {
                continue;
            };
            let updated = chrono::DateTime::parse_from_rfc3339(&state.updated_at)
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if latest.as_ref().is_none_or(|(at, _)| updated > *at) {
                latest = Some((updated, watermark));
            }
        }
        Ok(latest.map(|(_, watermark)| watermark))
    }
}
