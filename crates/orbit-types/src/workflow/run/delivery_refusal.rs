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
    /// Completion followed a moving base as many times as one run permits.
    /// Keep its certified recovery and resume on the next authorized owner
    /// drain pass, with a fresh per-run chase budget.
    BaseChaseExhausted,
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
    /// An in-budget `--auto-fix` check repair was admitted, and the job has
    /// no `step_failure_recovery` to run it. The run holds; the repair stays
    /// uncommitted. Spending the repair budget is still a failure.
    CheckRepairActivityAbsent,
    /// The forge started no run of any required check on the head Orbit
    /// pushed, and the one re-trigger completion requested for that head
    /// produced none either. The task stays in review; no repair is spent.
    CheckRunsAbsent,
    /// A `verify-mock-ui` delivery's capture was not established. Visual
    /// review was removed [ORB-22807]: nothing raises this any more, and it
    /// stays so runs recorded before then still read their refusal.
    VisualNotEstablished,
    /// A live visual review left a finding unresolved. Retired with visual
    /// review [ORB-22807], like [`Self::VisualNotEstablished`].
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
            Self::BaseChaseExhausted => "base_chase_exhausted",
            Self::CheckEvidenceInconclusive => "delivery_check_evidence_inconclusive",
            Self::ForgeObservationUnavailable => "delivery_forge_observation_unavailable",
            Self::OriginUnavailable => "delivery_origin_unavailable",
            Self::VisualNotEstablished => "delivery_visual_not_established",
            Self::VisualReviewUnresolved => "delivery_visual_review_unresolved",
            Self::CheckRepairActivityAbsent => "delivery_check_repair_activity_absent",
            Self::CheckRunsAbsent => "delivery_check_runs_absent",
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
            "base_chase_exhausted" => Some(Self::BaseChaseExhausted),
            "delivery_check_evidence_inconclusive" => Some(Self::CheckEvidenceInconclusive),
            "delivery_forge_observation_unavailable" => Some(Self::ForgeObservationUnavailable),
            "delivery_origin_unavailable" => Some(Self::OriginUnavailable),
            "delivery_visual_not_established" => Some(Self::VisualNotEstablished),
            "delivery_visual_review_unresolved" => Some(Self::VisualReviewUnresolved),
            "delivery_check_repair_activity_absent" => Some(Self::CheckRepairActivityAbsent),
            "delivery_check_runs_absent" => Some(Self::CheckRunsAbsent),
            _ => None,
        }
    }

    /// The operator's next step for this refusal, as the delivery-refusal
    /// reference states it. `run_id` fills the resume command for a forge
    /// observation hold; other refusals do not resume the held run.
    #[must_use]
    pub fn operator_next_step(self, run_id: &str) -> String {
        match self {
            Self::HeadChanged => {
                "Compare the pull-request head against the published candidate, then start a new delivery attempt on the current head.".into()
            }
            Self::EvidenceStale => {
                "Re-establish the pinned identity and confirm the merge evidence, then start a new delivery attempt.".into()
            }
            Self::RepairDispatched => {
                "If a repair is running, let it finish. If none ran because the job lacked step_failure_recovery, configure recovery for future heads; if it ran and failed, inspect the recovery attempt. Then repair and publish a new head and start a new delivery attempt.".into()
            }
            Self::PullRequestDraft => {
                "Mark the pull request ready for review, then start a new delivery attempt.".into()
            }
            Self::PullRequestClosed => {
                "Reopen the pull request or open a new one, then start a new delivery attempt.".into()
            }
            Self::TaskNotPromotable => {
                "Move the task to a promotable status, then start a new delivery attempt.".into()
            }
            Self::LandingConflict => {
                "Repair the conflict, revalidate, and hand off again.".into()
            }
            Self::RequirementsUnsatisfied => {
                "Satisfy the named gate. A stopped authorized owner landing with typed waiting_gate evidence resumes automatically after a fresh observation confirms the gate cleared, for at most three retries with 60/120/240-second minimum backoffs; explicit retries count toward that ceiling. Use Retry landing or orbit task retry-handoff for other stops and after the automatic retry budget is exhausted. Other deliveries need a new attempt.".into()
            }
            Self::BaseChecksPending => {
                "Wait for the base's own check result; do not run candidate recovery first.".into()
            }
            Self::CompletionBudgetExpired => {
                "Check the forge for the current pull-request state, then start a new delivery attempt.".into()
            }
            Self::BaseChaseExhausted => {
                "Resume on the next authorized owner drain pass. This run keeps its certified recovery and a fresh per-run chase budget.".into()
            }
            Self::CheckEvidenceInconclusive => {
                "Let the next auto-fix pass reread the pull-request status; do not dispatch a repair on that evidence.".into()
            }
            Self::ForgeObservationUnavailable => format!(
                "Run `orbit job resume {run_id}`: it reads the forge again and continues completion from its checkpoints."
            ),
            Self::OriginUnavailable => {
                "Wait for the origin to recover; a later normal admission retries the work. A task already in progress follows ordinary run failure handling instead.".into()
            }
            Self::CheckRepairActivityAbsent => {
                "Adding step_failure_recovery and retrying the same head holds again as delivery_repair_dispatched; it does not run the repair. Configure recovery for future heads, repair and publish a new head, then start a new delivery attempt. A new head can receive another repair only while the per-PR budget remains; exhausting it still fails the run.".into()
            }
            Self::VisualNotEstablished | Self::VisualReviewUnresolved => {
                "Inspect the held run. This refusal is no longer raised.".into()
            }
        }
    }
}
