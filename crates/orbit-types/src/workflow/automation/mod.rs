//! Shared delivery-trigger, batch and coverage contracts [ORB-11330].

pub mod members;
pub mod recovery;

mod batch;
mod delivery;
mod evidence;
mod ownership;
mod state;

pub use batch::{
    ActionReissue, BatchAttempt, BatchState, BatchWaiver, CoverageBatch, WaiveBatchRequest,
};
pub use delivery::{
    CoverageClass, Delivery, DeliveryExclusion, DeliveryTrigger, ExcludedDelivery, SourcePage,
    SourceRevision, UNATTRIBUTED_NO_LANDING_TASK, UNATTRIBUTED_TASKS_UNREADABLE,
};
pub use evidence::{
    AcceptedCoverage, COVERAGE_ARTIFACT, CoverageEvidence, CoverageReceiptSummary,
    EVIDENCE_AUTHORITY_ARTIFACT, EvidenceSubmission, ExaminationCheck, evidence_template,
};
pub use ownership::{DeliveryAssociation, DeliveryOwnership, DirectLandingRequest, OwnerAuthority};
pub use state::{AutomationDiagnostic, AutomationState};
