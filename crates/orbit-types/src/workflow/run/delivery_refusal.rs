//! Fail-closed delivery decisions that hold a run rather than fail it.

use serde::{Deserialize, Serialize};

/// A deliberate completion or landing refusal. Only the raising boundary
/// assigns these values; diagnostic text alone never establishes a hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryRefusal {
    /// The forge reports a head different from the candidate this run published.
    HeadChanged,
    /// The blocked head already has a dispatched repair.
    RepairDispatched,
    /// The pull request is still a draft.
    PullRequestDraft,
    /// The pull request closed without merging.
    PullRequestClosed,
    /// The task's current status does not permit promotion.
    TaskNotPromotable,
    /// Owner landing requires a freshly validated conflict repair.
    LandingConflict,
    /// Required reviews or checks remain unsatisfied, without a red check.
    RequirementsUnsatisfied,
    /// Failed checks require the base's own result before candidate recovery.
    BaseChecksPending,
    /// The completion deadline expired before the PR merged.
    /// The PR stays open and the task stays in review.
    CompletionBudgetExpired,
    /// An auto-fix stop whose check evidence could not be read conclusively.
    CheckEvidenceInconclusive,
}

impl DeliveryRefusal {
    /// The stable diagnostic code of this refusal.
    #[must_use]
    pub const fn error_code(self) -> &'static str {
        match self {
            Self::HeadChanged => "delivery_head_changed",
            Self::RepairDispatched => "delivery_repair_dispatched",
            Self::PullRequestDraft => "delivery_pr_draft",
            Self::PullRequestClosed => "delivery_pr_closed",
            Self::TaskNotPromotable => "delivery_task_not_promotable",
            Self::LandingConflict => "delivery_landing_conflict",
            Self::RequirementsUnsatisfied => "delivery_requirements_unsatisfied",
            Self::BaseChecksPending => "delivery_base_checks_pending",
            Self::CompletionBudgetExpired => "delivery_completion_budget_expired",
            Self::CheckEvidenceInconclusive => "delivery_check_evidence_inconclusive",
        }
    }

    /// Read a recorded diagnostic code, never a diagnostic message or prefix.
    #[must_use]
    pub fn from_error_code(code: &str) -> Option<Self> {
        match code {
            "delivery_head_changed" => Some(Self::HeadChanged),
            "delivery_repair_dispatched" => Some(Self::RepairDispatched),
            "delivery_pr_draft" => Some(Self::PullRequestDraft),
            "delivery_pr_closed" => Some(Self::PullRequestClosed),
            "delivery_task_not_promotable" => Some(Self::TaskNotPromotable),
            "delivery_landing_conflict" => Some(Self::LandingConflict),
            "delivery_requirements_unsatisfied" => Some(Self::RequirementsUnsatisfied),
            "delivery_base_checks_pending" => Some(Self::BaseChecksPending),
            "delivery_completion_budget_expired" => Some(Self::CompletionBudgetExpired),
            "delivery_check_evidence_inconclusive" => Some(Self::CheckEvidenceInconclusive),
            _ => None,
        }
    }
}
