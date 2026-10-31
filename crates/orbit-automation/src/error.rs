//! Typed failures from deterministic automation rules.

use orbit_common::OrbitError;
use std::{fmt, sync::Arc};

/// In-process deferral codes. Display preserves the historical diagnostic text.
///
/// Construct a named reason at its producer; never classify its rendered text.
#[derive(Debug, Clone)]
pub enum DeferredReason {
    /// `attempt_exhausted`.
    AttemptExhausted,
    /// `cancellation_withheld`.
    CancellationWithheld,
    /// `claim_missing`.
    ClaimMissing,
    /// `claim_superseded`.
    ClaimSuperseded,
    /// `concurrent_evaluation`.
    ConcurrentEvaluation,
    /// `delivery_boundary_missing`.
    DeliveryBoundaryMissing,
    /// `delivery_crosses_boundary`.
    DeliveryCrossesBoundary,
    /// `delivery_evidence_changed`.
    DeliveryEvidenceChanged,
    /// `delivery_grouping_ambiguous`.
    DeliveryGroupingAmbiguous,
    /// `dependency_scan_budget`.
    DependencyScanBudget,
    /// `empty_batch`.
    EmptyBatch,
    /// `foreign_run`.
    ForeignRun,
    /// `generation_exhausted`.
    GenerationExhausted,
    /// `history_diverged`.
    HistoryDiverged,
    /// `human_block`.
    HumanBlock,
    /// `human_intent_or_coupling_changed`.
    HumanIntentOrCouplingChanged,
    /// `incident_inventory_budget`.
    IncidentInventoryBudget,
    /// `incident_lineage_budget`.
    IncidentLineageBudget,
    /// `incident_lineage_cycle`.
    IncidentLineageCycle,
    /// `incident_lineage_missing`.
    IncidentLineageMissing,
    /// `incident_scan_budget`.
    IncidentScanBudget,
    /// `incident_unresolved`.
    IncidentUnresolved,
    /// `instruction_scan_budget`.
    InstructionScanBudget,
    /// `job_input_mismatch`.
    JobInputMismatch,
    /// `missing_claim`.
    MissingClaim,
    /// `not_execution_failure`.
    NotExecutionFailure,
    /// `provider_identity_missing`.
    ProviderIdentityMissing,
    /// `provider_response_invalid`.
    ProviderResponseInvalid,
    /// `recovery_pending`.
    RecoveryPending,
    /// `repository_changed`.
    RepositoryChanged,
    /// `selector_scan_budget`.
    SelectorScanBudget,
    /// `source_backpressure`.
    SourceBackpressure,
    /// `source_budget`.
    SourceBudget,
    /// `source_count_invalid`.
    SourceCountInvalid,
    /// `source_deadline`.
    SourceDeadline,
    /// `stream did not contain valid UTF-8`.
    SourceInvalidUtf8,
    /// `source_membership_changed`.
    SourceMembershipChanged,
    /// `source_page_invalid`.
    SourcePageInvalid,
    /// `source_revision_changed`.
    SourceRevisionChanged,
    /// `state_missing`.
    StateMissing,
    /// Evidence command or local Git inspection failed, with its original detail.
    EvidenceUnavailable(String),
    /// The command could not be spawned, with command and operating-system detail.
    SourceSpawnFailed(String),
    /// A fetch failed; absent detail renders the bare code.
    SourceFetchFailed(Option<String>),
    /// The pin owner could not be proven, with its original detail.
    AutomationPinOwnerUnproven(String),
    /// A source pin no longer names its attempt's commit.
    AutomationPinMoved {
        /// The pin ref.
        refname: String,
        /// The commit currently pinned.
        pinned: String,
        /// The commit the attempt froze.
        commit: String,
    },
    /// A source pin was not written.
    AutomationPinUnwritten {
        /// The missing pin ref.
        refname: String,
    },
    /// The task inventory could not be read, with its original detail.
    TaskRecordsUnavailable(String),
    /// Legacy unprefixed source I/O diagnostics; retain the concrete error.
    SourceIo(Arc<std::io::Error>),
    /// Legacy unprefixed provider JSON diagnostics; retain the concrete error.
    ProviderResponseDecode(Arc<serde_json::Error>),
    /// Legacy unprefixed child-supervision diagnostics.
    SourceSupervision(Arc<OrbitError>),
    /// A non-deferred source-cache failure historically forwarded as a deferral.
    CachedFailure(Arc<AutomationError>),
}

impl fmt::Display for DeferredReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AttemptExhausted => f.write_str("attempt_exhausted"),
            Self::CancellationWithheld => f.write_str("cancellation_withheld"),
            Self::ClaimMissing => f.write_str("claim_missing"),
            Self::ClaimSuperseded => f.write_str("claim_superseded"),
            Self::ConcurrentEvaluation => f.write_str("concurrent_evaluation"),
            Self::DeliveryBoundaryMissing => f.write_str("delivery_boundary_missing"),
            Self::DeliveryCrossesBoundary => f.write_str("delivery_crosses_boundary"),
            Self::DeliveryEvidenceChanged => f.write_str("delivery_evidence_changed"),
            Self::DeliveryGroupingAmbiguous => f.write_str("delivery_grouping_ambiguous"),
            Self::DependencyScanBudget => f.write_str("dependency_scan_budget"),
            Self::EmptyBatch => f.write_str("empty_batch"),
            Self::ForeignRun => f.write_str("foreign_run"),
            Self::GenerationExhausted => f.write_str("generation_exhausted"),
            Self::HistoryDiverged => f.write_str("history_diverged"),
            Self::HumanBlock => f.write_str("human_block"),
            Self::HumanIntentOrCouplingChanged => f.write_str("human_intent_or_coupling_changed"),
            Self::IncidentInventoryBudget => f.write_str("incident_inventory_budget"),
            Self::IncidentLineageBudget => f.write_str("incident_lineage_budget"),
            Self::IncidentLineageCycle => f.write_str("incident_lineage_cycle"),
            Self::IncidentLineageMissing => f.write_str("incident_lineage_missing"),
            Self::IncidentScanBudget => f.write_str("incident_scan_budget"),
            Self::IncidentUnresolved => f.write_str("incident_unresolved"),
            Self::InstructionScanBudget => f.write_str("instruction_scan_budget"),
            Self::JobInputMismatch => f.write_str("job_input_mismatch"),
            Self::MissingClaim => f.write_str("missing_claim"),
            Self::NotExecutionFailure => f.write_str("not_execution_failure"),
            Self::ProviderIdentityMissing => f.write_str("provider_identity_missing"),
            Self::ProviderResponseInvalid => f.write_str("provider_response_invalid"),
            Self::RecoveryPending => f.write_str("recovery_pending"),
            Self::RepositoryChanged => f.write_str("repository_changed"),
            Self::SelectorScanBudget => f.write_str("selector_scan_budget"),
            Self::SourceBackpressure => f.write_str("source_backpressure"),
            Self::SourceBudget => f.write_str("source_budget"),
            Self::SourceCountInvalid => f.write_str("source_count_invalid"),
            Self::SourceDeadline => f.write_str("source_deadline"),
            Self::SourceInvalidUtf8 => f.write_str("stream did not contain valid UTF-8"),
            Self::SourceMembershipChanged => f.write_str("source_membership_changed"),
            Self::SourcePageInvalid => f.write_str("source_page_invalid"),
            Self::SourceRevisionChanged => f.write_str("source_revision_changed"),
            Self::StateMissing => f.write_str("state_missing"),
            Self::EvidenceUnavailable(detail) => write!(f, "evidence_unavailable: {detail}"),
            Self::SourceSpawnFailed(detail) => write!(f, "source_spawn_failed: {detail}"),
            Self::AutomationPinOwnerUnproven(detail) => {
                write!(f, "automation_pin_owner_unproven: {detail}")
            }
            Self::AutomationPinMoved {
                refname,
                pinned,
                commit,
            } => write!(
                f,
                "automation_pin_moved: {refname} names {pinned}, not the attempt's {commit}"
            ),
            Self::AutomationPinUnwritten { refname } => {
                write!(f, "automation_pin_unwritten: {refname}")
            }
            Self::TaskRecordsUnavailable(detail) => write!(f, "task_records_unavailable: {detail}"),
            Self::SourceFetchFailed(None) => f.write_str("source_fetch_failed"),
            Self::SourceFetchFailed(Some(detail)) => write!(f, "source_fetch_failed: {detail}"),
            Self::SourceIo(error) => fmt::Display::fmt(error, f),
            Self::ProviderResponseDecode(error) => fmt::Display::fmt(error, f),
            Self::SourceSupervision(error) => fmt::Display::fmt(error, f),
            Self::CachedFailure(error) => fmt::Display::fmt(error, f),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AutomationError {
    #[error("invalid coverage evidence: {0}")]
    Evidence(String),
    #[error("automation deferred: {0}")]
    Deferred(DeferredReason),
    /// An explicit operator operation the current state forbids, naming every
    /// refusal that applies. Nothing was changed.
    #[error("recovery refused: {0}")]
    Refused(String),
    #[error(transparent)]
    Boundary(#[from] OrbitError),
}

/// Translate once at the Core boundary, preserving invalid evidence as input errors.
pub fn automation_error_to_orbit(error: AutomationError) -> OrbitError {
    match error {
        AutomationError::Evidence(reason) => {
            OrbitError::InvalidInput(format!("coverage_evidence_invalid: {reason}"))
        }
        AutomationError::Deferred(reason) => {
            OrbitError::Execution(format!("automation_deferred: {reason}"))
        }
        AutomationError::Refused(reasons) => {
            OrbitError::InvalidInput(format!("recovery_refused: {reasons}"))
        }
        AutomationError::Boundary(error) => error,
    }
}
