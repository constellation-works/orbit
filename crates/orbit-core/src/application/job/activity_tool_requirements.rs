//! Which activities consume a task's required-tool contract, and which tools
//! a requirement or an experiment arm may name.
//!
//! The application owns this policy; the runtime-host adapter applies it
//! while translating a tool grant for the engine.

use orbit_common::OrbitError;
use orbit_types::tool::is_exact_canonical_tool_name;

use crate::OrbitRuntime;

/// Shipped agent activities that are not the task's implementer.
///
/// These activities still validate every requirement, but a tool their
/// disallow list covers is omitted. A name absent from this list, including
/// a custom implementer, keeps fail-closed admission. Add a new shipped
/// recovery or review activity here when it should start even though the
/// task requires a tool it disallows.
const NON_IMPLEMENTER_ACTIVITIES: &[&str] = &[
    "agent_explore",
    "agent_invoke",
    "agent_review_repair",
    "final_recovery",
    "pr_conflict_recovery",
    "review_reconciliation_review",
    "step_failure_recovery",
    "task_pilot",
    "triage_escalation",
    "verify_finding",
];

/// Whether a deny-mode activity omits covered task requirements rather than
/// refusing admission. Implementers retain the fail-closed default.
pub(crate) fn drops_disallowed_requirements(activity: &str) -> bool {
    NON_IMPLEMENTER_ACTIVITIES.contains(&activity)
}

/// Why `tool_name` cannot be granted beyond an activity's baseline: it is not
/// an exact canonical name, or not a registered, agent-facing, enabled tool.
/// `None` when it can be. Task `required_tools` and experiment arm additions
/// are admitted by this one rule.
pub(crate) fn granted_tool_refusal(
    runtime: &OrbitRuntime,
    tool_name: &str,
) -> Result<Option<String>, OrbitError> {
    let reason = if tool_name.contains('*') {
        "wildcard and prefix requirements are not allowed"
    } else if !is_exact_canonical_tool_name(tool_name) {
        "malformed canonical tool name"
    } else if !runtime.tool_registry().has(tool_name) {
        "unknown registered tool"
    } else if !runtime.tool_registry().is_active(tool_name) {
        "tool is not agent-facing"
    } else if runtime
        .stores()
        .tools()
        .get_tool(tool_name)?
        .is_some_and(|tool| !tool.enabled)
    {
        "tool is inactive"
    } else {
        return Ok(None);
    };
    Ok(Some(reason.to_string()))
}
