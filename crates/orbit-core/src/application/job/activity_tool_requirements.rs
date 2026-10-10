//! Which activities consume a task's required-tool contract.
//!
//! The application owns this policy; the runtime-host adapter applies it
//! while translating a tool grant for the engine.

/// Shipped agent activities that are not the task's implementer.
///
/// These activities still validate every requirement, but a tool their
/// disallow list covers is omitted. A name absent from this list, including
/// a custom implementer, keeps fail-closed admission. Add a new shipped
/// recovery or review activity here when it should start even though the
/// task requires a tool it disallows.
const NON_IMPLEMENTER_ACTIVITIES: &[&str] = &[
    "agent_invoke",
    "agent_review_repair",
    "final_recovery",
    "pr_conflict_recovery",
    "review_reconciliation_review",
    "step_failure_recovery",
    "task_pilot",
];

/// Whether a deny-mode activity omits covered task requirements rather than
/// refusing admission. Implementers retain the fail-closed default.
pub(crate) fn drops_disallowed_requirements(activity: &str) -> bool {
    NON_IMPLEMENTER_ACTIVITIES.contains(&activity)
}
