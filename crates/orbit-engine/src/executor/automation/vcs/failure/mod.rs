mod conflict;
mod handoff;
mod ownership;
mod preserve;
mod review_gate;

pub(super) use super::resume::commit_head_matches_failure_handoff;
pub(in crate::executor::automation) use handoff::pr_failure_handoff;

const CONFLICT_BLOCKED_EVENT: &str = "pr_conflict_blocked";

const FAILURE_HANDOFF_EVENT: &str = "pr_failure_handoff";

/// The handoff found a rebase this run did not start and left it intact [ORB-13455].
const FOREIGN_REBASE_EVENT: &str = "pr_foreign_rebase_refused";

/// A before-PR review gate stopped delivery [ORB-11333].
const REVIEW_GATE_EVENT: &str = "review_gate_escalation";

/// Required validation could not find a tool; the candidate was not judged
/// [ORB-13987].
const VALIDATION_ENVIRONMENT_EVENT: &str = "validation_environment_blocked";

/// Largest validation diagnostic the blocking comment repeats; the full
/// output is in the attached validation log.
const MAX_VALIDATION_ENVIRONMENT_DIAGNOSTIC_BYTES: usize = 16 * 1024;

/// The pipeline steps that belong to the before-PR review gate: admission,
/// the reviewer, settlement, and owner revalidation of the reviewer's fixes
/// [ORB-13989].
pub(in crate::executor::automation) const REVIEW_GATE_STEPS: &[&str] = &[
    "review_gate_admit",
    "review",
    "review_gate_settle",
    REVIEW_VALIDATION_STEP,
];

/// Owner revalidation of the reviewer commit. Its failure rejects the
/// candidate: there is no second review round [ORB-13989].
const REVIEW_VALIDATION_STEP: &str = "review_validate";

/// Completion-stage steps: merging the published PR, and re-reviewing and
/// republishing it after completion rebased a conflicting reviewed head —
/// up to two rounds, when the base moved again during the first.
const COMPLETION_STEPS: &[&str] = &[
    "complete_pr",
    "re_review_gate_admit",
    "re_review",
    "re_review_gate_settle",
    "re_review_validate",
    "re_push",
    "complete_reviewed_pr",
    "re_review_gate_admit_2",
    "re_review_2",
    "re_review_gate_settle_2",
    "re_review_validate_2",
    "re_push_2",
    "complete_reviewed_pr_2",
];

/// Admission checkpoints whose attempt a failing run must close.
const REVIEW_ADMISSION_STEPS: &[&str] = &[
    "review_gate_admit",
    "re_review_gate_admit",
    "re_review_gate_admit_2",
];
