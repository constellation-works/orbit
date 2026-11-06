//! What task decision operations need from the precedent consumer
//! [ORB-22313].
//!
//! A raised request is offered to the consumer, a human's review of a model
//! answer feeds its rolling window, and the decision digest reports its demoted
//! categories. `precedent_decision` owns all of that, and it also applies task
//! answers, so a direct call from here would close a cycle. This port is the
//! way in: `precedent_decision` implements it on
//! [`OrbitRuntime`](crate::OrbitRuntime), so composing the runtime binds it.

use orbit_types::task::DecisionRequest;

use crate::application::contracts::PrecedentAssistEnablement;

/// The precedent-consumer operations task code calls, implemented by
/// `precedent_decision`.
pub(in crate::application) trait TaskPrecedentDecisionPort {
    /// Best-effort enqueue for a request a task write left open. A consumer
    /// failure does not fail the write that raised it.
    fn enqueue_open_decision_for_precedent(&self, request: &DecisionRequest);

    /// A human confirmed or vetoed the model answer to `original`. Best
    /// effort: the review's own write never fails on the window.
    fn record_model_answer_review_outcome(&self, original: &DecisionRequest, vetoed: bool);

    /// This workspace's demoted categories, for the decision digest. Empty
    /// when the store is unreadable from this process.
    fn demoted_precedent_categories(&self) -> Vec<PrecedentAssistEnablement>;
}
