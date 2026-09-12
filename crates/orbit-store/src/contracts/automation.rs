//! Atomic scheduler checkpoints and immutable accepted coverage.

use orbit_common::OrbitError;
use orbit_types::workflow::automation::recovery::RecoveryRecord;
use orbit_types::workflow::automation::{AcceptedCoverage, AutomationState, BatchWaiver, Delivery};

pub trait AutomationStoreBackend: Send + Sync {
    fn automation_waive(
        &self,
        _previous: &AutomationState,
        _next: &AutomationState,
        _waiver: &BatchWaiver,
    ) -> Result<bool, OrbitError> {
        Err(OrbitError::Store(
            "batch waiver persistence unavailable".into(),
        ))
    }

    fn automation_waivers(
        &self,
        _consumer: &str,
        _limit: usize,
    ) -> Result<Vec<BatchWaiver>, OrbitError> {
        Ok(vec![])
    }

    /// Adopt a new configuration identity and/or an authorized reissue under
    /// the same generation fence, writing the audit record in one transaction.
    /// The checkpoint may move nothing else: every cursor, obligation and
    /// accepted fact is carried over unchanged.
    fn automation_recover(
        &self,
        _previous: &AutomationState,
        _next: &AutomationState,
        _record: &RecoveryRecord,
    ) -> Result<bool, OrbitError> {
        Err(OrbitError::Store(
            "consumer recovery persistence unavailable".into(),
        ))
    }

    /// Forget one consumer's state entirely and write the audit record that
    /// says what was forgotten, in one transaction under the same generation
    /// fence. The next evaluation seeds a fresh baseline at the branch head.
    fn automation_reset(
        &self,
        _previous: &AutomationState,
        _record: &RecoveryRecord,
    ) -> Result<bool, OrbitError> {
        Err(OrbitError::Store(
            "consumer reset persistence unavailable".into(),
        ))
    }

    /// Record or clear the consumer's stall marker under the generation
    /// fence. Nothing but the marker may move.
    fn automation_stall(
        &self,
        _previous: &AutomationState,
        _next: &AutomationState,
    ) -> Result<bool, OrbitError> {
        Err(OrbitError::Store(
            "consumer stall persistence unavailable".into(),
        ))
    }

    /// Audited recoveries for a consumer, newest first.
    fn automation_recoveries(
        &self,
        _consumer: &str,
        _limit: usize,
    ) -> Result<Vec<RecoveryRecord>, OrbitError> {
        Ok(vec![])
    }

    fn automation_receipt(
        &self,
        consumer: &str,
        batch: &str,
    ) -> Result<Option<AcceptedCoverage>, OrbitError> {
        Ok(self
            .automation_receipts(consumer, 100)?
            .into_iter()
            .find(|r| r.batch_id == batch))
    }

    fn automation_record_delivery_intent(&self, _delivery: &Delivery) -> Result<(), OrbitError> {
        Err(OrbitError::Store(
            "delivery intent persistence unavailable".into(),
        ))
    }

    fn automation_delivery_intents(
        &self,
        _repository: &str,
        _branch: &str,
        _commits: &[String],
    ) -> Result<Vec<Delivery>, OrbitError> {
        Ok(vec![])
    }

    fn automation_state(&self, consumer: &str) -> Result<Option<AutomationState>, OrbitError>;
    /// Every persisted consumer state whose key starts with `prefix`, bounded
    /// to `limit`. Read-only: operation-mode promotion consumes accepted
    /// assessments from here rather than re-deriving readiness [ORB-11332].
    fn automation_states(
        &self,
        _prefix: &str,
        _limit: usize,
    ) -> Result<Vec<AutomationState>, OrbitError> {
        Ok(vec![])
    }
    /// Inserts once; a missing state is never silently substituted for corrupt data.
    fn automation_initialize(&self, state: &AutomationState) -> Result<bool, OrbitError>;
    /// Generation-fenced checkpoint and optional receipt commit in one transaction.
    fn automation_commit(
        &self,
        previous: &AutomationState,
        next: &AutomationState,
        receipt: Option<&AcceptedCoverage>,
    ) -> Result<bool, OrbitError>;
    fn automation_receipts(
        &self,
        consumer: &str,
        limit: usize,
    ) -> Result<Vec<AcceptedCoverage>, OrbitError>;
}
