//! Atomic scheduler checkpoints and immutable accepted coverage.

use orbit_common::OrbitError;
use orbit_types::workflow::automation::recovery::RecoveryRecord;
use orbit_types::workflow::automation::{
    AcceptedCoverage, AutomationState, BatchWaiver, ConditionAudit, ConditionState, Delivery,
};

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
    /// A bounded page of states whose consumer keys start with the literal
    /// `prefix`, ordered by consumer key, strictly after `after` when given.
    /// `limit` must be nonzero; implementations may cap the page size further.
    /// Continue from the last returned key until an empty page proves exhaustion,
    /// even when a page contains fewer than `limit` states. An implementation
    /// that cannot provide this complete traversal must return an error.
    /// Pages observe current persisted state, not a snapshot across calls.
    fn automation_states_page(
        &self,
        _prefix: &str,
        _after: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<AutomationState>, OrbitError> {
        Err(OrbitError::Store(
            "paginated automation state inspection unavailable".into(),
        ))
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

    /// One condition consumer's state. A row that does not decode, or carries
    /// another schema version, is an error, never `None`: missing state would
    /// read as a fresh baseline.
    fn condition_state(&self, _consumer: &str) -> Result<Option<ConditionState>, OrbitError> {
        Err(OrbitError::Store(
            "condition state persistence unavailable".into(),
        ))
    }

    /// Condition states whose consumer keys start with the literal `prefix`,
    /// ordered by key, strictly after `after`, at most `limit` (capped at 100).
    fn condition_states_page(
        &self,
        _prefix: &str,
        _after: Option<&str>,
        _limit: usize,
    ) -> Result<Vec<ConditionState>, OrbitError> {
        Err(OrbitError::Store(
            "condition state persistence unavailable".into(),
        ))
    }

    /// Insert a consumer's first state at generation 0. `false` when another
    /// writer inserted it first.
    fn condition_initialize(&self, _state: &ConditionState) -> Result<bool, OrbitError> {
        Err(OrbitError::Store(
            "condition state persistence unavailable".into(),
        ))
    }

    /// Replace `previous` with `next` when the stored row is still exactly
    /// `previous` at its generation and `next` is the following generation.
    /// `false` means another evaluator moved it: this one writes nothing.
    fn condition_commit(
        &self,
        _previous: &ConditionState,
        _next: &ConditionState,
    ) -> Result<bool, OrbitError> {
        Err(OrbitError::Store(
            "condition state persistence unavailable".into(),
        ))
    }

    /// [`Self::condition_commit`] and the audit record of the operator action
    /// that made it, in one transaction.
    fn condition_commit_audited(
        &self,
        _previous: &ConditionState,
        _next: &ConditionState,
        _audit: &ConditionAudit,
    ) -> Result<bool, OrbitError> {
        Err(OrbitError::Store(
            "condition audit persistence unavailable".into(),
        ))
    }

    /// Audited operator actions on a condition consumer, newest first.
    fn condition_audits(
        &self,
        _consumer: &str,
        _limit: usize,
    ) -> Result<Vec<ConditionAudit>, OrbitError> {
        Ok(vec![])
    }

    /// Forget a deleted definition's condition state. Its audit stays.
    fn condition_delete(&self, _consumer: &str) -> Result<(), OrbitError> {
        Err(OrbitError::Store(
            "condition state persistence unavailable".into(),
        ))
    }
}
