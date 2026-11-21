//! Exact delivery evidence and owner completion authority for distributed handoffs.
//! These records execute no merge; a before-PR review they carry is evidence
//! the owner verifies, never authority on its own.
//!
//! Records group by delivery phase: `delivery` (candidate, review, task
//! handoff, validation and authorization), `landing` (attempts, stop
//! evidence, event names and retries) and `clean_tree` (already-landed,
//! no-diff and finding-invalid evidence, already-resolved and scope
//! comparison). Every public item is re-exported here.

mod checks_observation;
mod clean_tree;
mod delivery;
mod landing;

pub use checks_observation::{
    HistoricalLandingChecksObservation, LandingChecksObservationReplayKey,
    LandingChecksObservationState, RecordedLandingChecksObservation,
};
pub use clean_tree::{
    ALREADY_RESOLVED_FIELD, AlreadyLandedCheck, AlreadyLandedEvidence, AlreadyResolved,
    FindingInvalidCitation, FindingInvalidReport, NoDiffCheck, NoDiffEvidence,
    already_landed_scope, already_landed_scope_matches,
};
pub use delivery::{
    AcceptedHandoff, HandoffArtifactRef, HandoffAuthorization, HandoffAuthorizationSource,
    HandoffCandidate, HandoffDelivery, HandoffKnownRed, HandoffReview, HandoffReviewDisposition,
    HandoffReviewEvidence, HandoffRevocation, HandoffValidationLog, TaskHandoff,
};
pub use landing::{
    EXTERNAL_MERGE_SETTLED_EVENT, LANDING_CHECKS_OBSERVED_EVENT, LANDING_REPAIR_BUDGET,
    LANDING_REPAIR_EXHAUSTED_EVENT, LANDING_REPAIR_REQUEUED_EVENT, LANDING_WAITING_FOR_BASE_EVENT,
    LandingAttempt, LandingAttemptState, LandingRefusalReason, LandingRetryReceipt,
    LandingRetryRequest, LandingStartRequest, LandingStartState, LandingStopEvidence,
    LandingWaitGate, OwnerLandingRefusal, OwnerLandingRefusalKind, landing_repairs_spent,
};
