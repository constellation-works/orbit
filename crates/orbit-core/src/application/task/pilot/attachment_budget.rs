//! Per-complexity selector budgets and the complexity an applied assessment writes.

use orbit_types::task::TaskComplexity;

/// Field carrying the deterministic over-attachment finding this boundary
/// injects into an applied assessment [ORB-12228]. It sits beside
/// [`super::VALIDATION_TOOL_WARNINGS`] so the orchestrator reads the agent's findings
/// and the host's in the same assessment.
pub(super) const CONTEXT_ATTACHMENT_WARNINGS: &str = "context_attachment_warnings";

/// Selector budget for a proposal at each recommended complexity.
///
/// Context selectors are both the executor's reading list and the task's lock
/// reservations, so a proposal far larger than the assessed repair serializes
/// unrelated work on the same files and hides the real modification targets.
/// Each budget sits well above the largest honest attachment observed for its
/// tier, which makes exceeding it a signal that the pilot swept a directory
/// instead of deriving targets from references. `unassessed` shares the
/// strictest budget: a pilot that could not size the repair has no evidence
/// for reserving a wide surface either.
fn context_selector_cap(complexity: TaskComplexity) -> usize {
    match complexity {
        TaskComplexity::Unassessed | TaskComplexity::Low => 10,
        TaskComplexity::Medium => 20,
        TaskComplexity::Hard => 40,
        TaskComplexity::XHard => 60,
    }
}

/// The complexity this boundary writes for an assessment.
///
/// The recommendation applies as-is, with one exception: a task already
/// carrying the top tier keeps it. `xhard` routes work to the most capable —
/// and most expensive — crews, so a task there receives its selectors from a
/// lower recommendation without being demoted out of that pool; lowering it
/// is an operator decision [ORB-12622].
pub(super) fn resolve_applied_complexity(
    recommended: TaskComplexity,
    current: Option<TaskComplexity>,
) -> TaskComplexity {
    match current {
        Some(TaskComplexity::XHard) => TaskComplexity::XHard,
        _ => recommended,
    }
}

/// Report an over-budget proposal without refusing it. Genuinely large-surface
/// work must stay applyable, so this returns a finding for the orchestrator to
/// weigh rather than a validation error [ORB-12228].
pub(super) fn over_attachment_findings(
    complexity: TaskComplexity,
    after: &[String],
) -> Vec<String> {
    let cap = context_selector_cap(complexity);
    if after.len() <= cap {
        return Vec::new();
    }
    vec![format!(
        "over-attached context: {} selectors proposed for {complexity} complexity, above the \
         {cap}-selector budget for that tier; context selectors are lock reservations and the \
         executor's reading list, so each one should be a file this task modifies",
        after.len()
    )]
}
