//! Independent review policy contracts [ORB-11333].
//!
//! A managed PR delivery may hold PR creation for a fresh reviewer. The
//! records here are the durable evidence that gate produces: the admission
//! snapshot a run captures, the manifest handed to the reviewer, the honest
//! verdict, the certificate that binds a passed verdict to exact base and
//! candidate trees, the mapping to the commit that actually landed, and the
//! per-lineage attempt ledger. None of these is a task status, a human
//! approval, or merge permission; they only describe what was examined.

mod admission;
mod certificate;
mod history;
mod ledger;
mod records;
mod report;
mod verdict;

#[cfg(test)]
mod tests;

pub use admission::{
    CommitIdentity, DEFAULT_REVIEW_MINUTES, REVIEW_ADMISSION_KEY, REVIEW_BASELINE_ARTIFACT,
    REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, REVIEW_MANIFEST_ARTIFACT,
    REVIEW_REPORT_ARTIFACT, ReviewAdmission, ReviewBudget, ReviewCrewPoolMember, ReviewTiming,
    is_reserved_review_artifact,
};
pub use certificate::{
    HostCandidateOverride, LandingTransformation, ReviewCertificate, ReviewConsumption,
    ReviewInvalidation, ReviewLanding, ReviewManifest, ReviewerIdentity,
};
pub use history::{
    REVIEW_REPORT_HISTORY_ARTIFACT, REVIEW_REPORT_HISTORY_LIMIT, REVIEW_REPORT_HISTORY_VERSION,
    ReviewReportHistory, ReviewReportRevision,
};
pub use ledger::{
    REVIEW_ABANDONED_MARKER, REVIEW_LANDING_DECISION_PENDING, ReviewAttempt, ReviewAttemptState,
    ReviewLedger, ReviewReservation, ReviewResetDecision, ReviewerInvocation,
    ReviewerInvocationEvent, seconds_between,
};
pub use records::{RecordGap, RetiredValidation, record_gap};
pub use report::{FindingDisposition, ReviewFinding, ReviewReport};
pub use verdict::{
    NegativeControl, RetainedObligation, ReviewAssurance, ReviewBaselineClaim, ReviewValidation,
    ReviewVerdict, ValidationOutcome, ValidationRole,
};
