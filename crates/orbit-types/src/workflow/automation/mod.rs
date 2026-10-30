//! Shared delivery-trigger, batch and coverage contracts [ORB-11330].

pub mod members;
pub mod recovery;

mod batch;
mod condition;
mod condition_state;
mod delivery;
mod evidence;
mod ownership;
mod state;

pub use batch::{
    ActionReissue, BatchAttempt, BatchState, BatchWaiver, CoverageBatch, WaiveBatchRequest,
};
pub use condition::{
    ConditionFileIdentity, ConditionGroupBy, ConditionGuardMiss, ConditionOnEnable, ConditionScope,
    ConditionTrigger, ScheduleGuard,
};
pub use condition_state::{
    CONDITION_ADMISSION_ATTEMPTS, CONDITION_FIRED_EVENT, CONDITION_FIRED_RETENTION_DAYS,
    CONDITION_OSCILLATION_FIRES_PER_HOUR, CONDITION_RETAINED_RECORDS,
    CONDITION_STATE_SCHEMA_VERSION, CONDITION_SUBJECTS_PER_PASS, CONDITION_WORKSPACE_SUBJECT,
    ConditionArm, ConditionAudit, ConditionFire, ConditionFireState, ConditionOrigin,
    ConditionPass, ConditionSkip, ConditionStall, ConditionState, ConditionSubject,
    MAX_CONDITION_CHAIN_DEPTH,
};
pub use delivery::{
    CoverageClass, Delivery, DeliveryExclusion, DeliveryTrigger, ExcludedDelivery, SourcePage,
    SourceRevision, UNATTRIBUTED_NO_LANDING_TASK, UNATTRIBUTED_TASKS_UNREADABLE,
};
pub use evidence::{
    AcceptedCoverage, COVERAGE_ARTIFACT, COVERAGE_EVIDENCE_SCHEMA_VERSION, ClaimedSubmission,
    CoverageEvidence, CoverageReceiptSummary, DeliveryExamination, DeliveryVerdict,
    EVIDENCE_AUTHORITY_ARTIFACT, EvidenceSubmission, ExaminationCheck, SkippedPath,
    evidence_template,
};
pub use ownership::{DeliveryAssociation, DeliveryOwnership, DirectLandingRequest, OwnerAuthority};
pub use state::{AssociationLookupRetry, AutomationDiagnostic, AutomationState};
