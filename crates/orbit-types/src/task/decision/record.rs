//! The [`TaskDecision`] record and the history events that carry its
//! final-recovery and pilot-admission variants.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    CiFailureIdentity, CodeScanningAlertEvidence, DesktopMutationReceipt, PilotAdmissionVerdict,
    PublicationSettlingObservation, RecordedFinalRecovery,
};
use crate::task::{ContextFilesWidening, RegressionRiskMarks, TagsChanged};
use crate::workflow::{FlakyValidation, HeldCandidate, KnownRedValidation};

/// History event the final-recovery applier records its decision on.
pub const FINAL_RECOVERY_DECIDED_EVENT: &str = "final_recovery_decided";
/// History event an operator's task-pilot admission decision is recorded on
/// when no typed hold has its own resolution event.
pub const PILOT_ADMISSION_DECIDED_EVENT: &str = "pilot_admission_decided";

/// A decision recorded on a task history event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskDecision {
    /// Idempotency receipt for a desktop mutation or its completed PR closure pass.
    DesktopMutation(DesktopMutationReceipt),
    /// What final recovery decided for one run, and what its applier did.
    FinalRecovery {
        /// The run the decision belongs to.
        run_id: String,
        /// The decision kind the agent proposed.
        decision: String,
        /// What the applier did with it.
        outcome: RecordedFinalRecovery,
    },
    /// An operator's decision on the task-pilot assessment a receipt sealed.
    PilotAdmission {
        /// Operation id of the pilot receipt the decision answers.
        operation_id: String,
        /// What the operator decided.
        verdict: PilotAdmissionVerdict,
        /// The typed hold admission reported when the operator decided, when
        /// it reported one; its scope bounds what the decision resolves.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hold: Option<Value>,
    },
    /// An auto drain withheld approval of the task its pilot assessed.
    DrainApprovalHeld {
        /// Why the drain held it.
        classification: String,
    },
    /// Where a candidate an owner-local run held is, recorded on the
    /// `candidate_held` event.
    CandidateHeld(HeldCandidate),
    /// Selectors appended for paths an agent changed, recorded on the
    /// `context_files_widened` event.
    ContextFilesWidened(ContextFilesWidening),
    /// A required command that failed only with tests its base fails too,
    /// recorded on the `known_red_validation` event.
    KnownRedValidation(KnownRedValidation),
    /// Tests a candidate failed and then passed in one isolated replay,
    /// recorded on the `flaky_validation` event.
    FlakyValidation(FlakyValidation),
    /// Context files a prompt marked as regression-prone, recorded on the
    /// `regression_risk_marked` event.
    RegressionRiskMarked(RegressionRiskMarks),
    /// A verified source-repair publication read and its durable wait reservation.
    SourceRepairPublication(PublicationSettlingObservation),
    /// The reason a reused PR stayed draft or became ready for review.
    PrDraftRecovery(crate::workflow::PrDraftRecovery),
    /// Code scanning alert evidence a per-alert sweep task was filed from,
    /// recorded on its `created` event.
    CodeScanningAlert(CodeScanningAlertEvidence),
    /// CI failure dedupe identity, recorded on the sweep task's `created` event.
    CiFailureIdentity(CiFailureIdentity),
    /// The tag lists before and after a write that changed them, recorded
    /// on the `tags_changed` event.
    TagsChanged(TagsChanged),
    /// The keyed job-step invocation a task was minted for, recorded on its
    /// `created` event. It resolves a lost creation reply to this task.
    JobStepInvocation(JobStepInvocation),
}

/// Creation receipt of a task an auto-task job step minted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobStepInvocation {
    /// `(run lineage, step id)` reservation key.
    pub action_key: String,
    /// Digest of the definition and inputs the step requested.
    pub input_digest: String,
    /// The auto-task definition the task was minted from.
    pub auto_task: String,
    /// The job execution and step that minted it.
    pub run_id: String,
    pub step_id: String,
}
