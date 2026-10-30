//! Fail-closed delivery decisions that hold a run rather than fail it.

use serde::{Deserialize, Serialize};

/// A deliberate delivery refusal that parks a run. Only the raising boundary
/// assigns these values; diagnostic text alone never establishes a hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryRefusal {
    /// The forge reports a head different from the candidate this run published.
    HeadChanged,
    /// Delivery evidence is missing, contradictory, or cannot be confirmed.
    EvidenceStale,
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
    /// The completion deadline expired while the last observed PR state was
    /// unmerged. The task stays in review; the PR may have merged since, through
    /// auto-merge or another actor, so the forge is authoritative.
    CompletionBudgetExpired,
    /// An auto-fix stop whose check evidence could not be read conclusively.
    CheckEvidenceInconclusive,
    /// Completion's read-only forge observations kept failing with a transport
    /// error before any completion mutation, past the bounded completion
    /// retry. Nothing changed on the forge; the task stays in review and the
    /// held run resumes its completion once the forge answers.
    ForgeObservationUnavailable,
    /// The origin could not be reached while backlog tasks were being admitted
    /// through `worktree_setup`. They remain in backlog for a later admission
    /// after transport recovers. A task already in progress follows ordinary
    /// run failure handling, or claim settlement for a claimed leaf, instead.
    OriginUnavailable,
    /// A `verify-mock-ui` delivery's capture is not established and
    /// `visual_review.require_established` holds it until an operator
    /// releases it or a host with a verified browser kit captures it.
    VisualNotEstablished,
    /// A `verify-mock-ui` delivery in `visual_review.mode = "live"` still has
    /// a confirmed finding at or above `visual_review.block_at` the code
    /// reviewer did not fix, or disputed without the operator accepting the
    /// dispute; the candidate is held for the operator.
    VisualReviewUnresolved,
}

impl DeliveryRefusal {
    /// The stable diagnostic code of this refusal.
    #[must_use]
    pub const fn error_code(self) -> &'static str {
        match self {
            Self::HeadChanged => "delivery_head_changed",
            Self::EvidenceStale => "delivery_evidence_stale",
            Self::RepairDispatched => "delivery_repair_dispatched",
            Self::PullRequestDraft => "delivery_pr_draft",
            Self::PullRequestClosed => "delivery_pr_closed",
            Self::TaskNotPromotable => "delivery_task_not_promotable",
            Self::LandingConflict => "delivery_landing_conflict",
            Self::RequirementsUnsatisfied => "delivery_requirements_unsatisfied",
            Self::BaseChecksPending => "delivery_base_checks_pending",
            Self::CompletionBudgetExpired => "delivery_completion_budget_expired",
            Self::CheckEvidenceInconclusive => "delivery_check_evidence_inconclusive",
            Self::ForgeObservationUnavailable => "delivery_forge_observation_unavailable",
            Self::OriginUnavailable => "delivery_origin_unavailable",
            Self::VisualNotEstablished => "delivery_visual_not_established",
            Self::VisualReviewUnresolved => "delivery_visual_review_unresolved",
        }
    }

    /// Read a recorded diagnostic code, never a diagnostic message or prefix.
    #[must_use]
    pub fn from_error_code(code: &str) -> Option<Self> {
        match code {
            "delivery_head_changed" => Some(Self::HeadChanged),
            "delivery_evidence_stale" => Some(Self::EvidenceStale),
            "delivery_repair_dispatched" => Some(Self::RepairDispatched),
            "delivery_pr_draft" => Some(Self::PullRequestDraft),
            "delivery_pr_closed" => Some(Self::PullRequestClosed),
            "delivery_task_not_promotable" => Some(Self::TaskNotPromotable),
            "delivery_landing_conflict" => Some(Self::LandingConflict),
            "delivery_requirements_unsatisfied" => Some(Self::RequirementsUnsatisfied),
            "delivery_base_checks_pending" => Some(Self::BaseChecksPending),
            "delivery_completion_budget_expired" => Some(Self::CompletionBudgetExpired),
            "delivery_check_evidence_inconclusive" => Some(Self::CheckEvidenceInconclusive),
            "delivery_forge_observation_unavailable" => Some(Self::ForgeObservationUnavailable),
            "delivery_origin_unavailable" => Some(Self::OriginUnavailable),
            "delivery_visual_not_established" => Some(Self::VisualNotEstablished),
            "delivery_visual_review_unresolved" => Some(Self::VisualReviewUnresolved),
            _ => None,
        }
    }
}
