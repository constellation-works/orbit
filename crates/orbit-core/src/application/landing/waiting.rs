//! Bounded owner-driven retries of waiting gates on the existing handoff.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::handoff::{LandingAttempt, LandingRetryRequest, LandingStopEvidence};

use super::LandingDispatch;
use crate::OrbitRuntime;

// Count all attempts, including explicit retries, so neither restarts nor
// alternating stop reasons replenish the automatic budget for this handoff.
const MAX_AUTOMATIC_LANDING_ATTEMPTS: u32 = 4;

impl OrbitRuntime {
    pub(super) fn retry_waiting_landing(
        &self,
        attempt: &LandingAttempt,
        now: DateTime<Utc>,
    ) -> Result<Option<LandingDispatch>, OrbitError> {
        if attempt.attempt >= MAX_AUTOMATIC_LANDING_ATTEMPTS
            || attempt.attempt == 0
            || now.signed_duration_since(attempt.updated_at).num_seconds()
                < 60 * (1_i64 << (attempt.attempt - 1))
            || self.landing_job_is_live(attempt)?
        {
            return Ok(None);
        }
        let Some(evidence) = attempt.evidence.as_deref() else {
            return Ok(None);
        };
        let LandingStopEvidence::OwnerRefusal(refusal) = LandingStopEvidence::parse(evidence)
        else {
            return Ok(None);
        };
        if refusal.repairable || refusal.requeue == Some(true) {
            return Ok(None);
        }
        let Some(gate) = refusal.waiting_gate else {
            return Ok(None);
        };
        let context = self.handoff_landing_context(&attempt.handoff_id)?;
        if !orbit_engine::landing_wait_gate_cleared(self, &context, gate)? {
            return Ok(None);
        }
        // Reserve atomically with exactly the same identity, authority and
        // fresh-candidate observation rules as explicit retry-handoff. The key
        // fences overlapping sweep/dispatch passes to one successor attempt.
        let request = LandingRetryRequest {
            handoff_id: attempt.handoff_id.clone(),
            expected_candidate_commit: context.candidate.candidate.commit,
            expected_base_commit: context.candidate.base.commit,
            expected_latest_attempt: attempt.attempt,
            expected_latest_state: attempt.state,
            reason: format!(
                "Automatic waiting-gate retry {}/3; stop_reason={}; landing_attempt={}",
                attempt.attempt,
                gate.stop_code(),
                attempt.attempt + 1
            ),
            request_key: format!("owner-waiting-gate:{}", attempt.attempt),
        };
        self.reserve_landing_retry(&self.workspace_id()?, &request, "owner-landing")
            .map(Some)
    }
}
