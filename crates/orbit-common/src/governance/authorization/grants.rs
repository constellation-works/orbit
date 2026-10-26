//! Reviewed grant policy beside the governed-operation catalog. Additions and
//! repeat-safety changes require the same authorization review as catalog rows.

use orbit_types::tool::McpCapability;

use super::catalog;

/// Whether a task grant can make a denied tool available to an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolGrantability {
    /// List membership may be granted; all other authorization still applies.
    Grantable,
    /// A grant cannot satisfy the tool's operator authority requirement.
    AlwaysOperatorOnly,
}

/// How a granted tool's effects must be handled by implementing activities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantedToolEffect {
    /// Repeating the same input has no additional effect; direct calls are safe.
    ReadRepeatSafe,
    /// The operation requires a reviewed mutation proposal, never a live call.
    Mutating,
}

/// The reviewed grantability and side-effect policy for one tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolGrantClassification {
    /// Whether an operator may place the tool in a task's grant list.
    pub grantability: ToolGrantability,
    /// Whether the granted operation is safe to repeat directly.
    pub effect: GrantedToolEffect,
}

const GRANTABLE_MUTATION: ToolGrantClassification = ToolGrantClassification {
    grantability: ToolGrantability::Grantable,
    effect: GrantedToolEffect::Mutating,
};
const OPERATOR_MUTATION: ToolGrantClassification = ToolGrantClassification {
    grantability: ToolGrantability::AlwaysOperatorOnly,
    effect: GrantedToolEffect::Mutating,
};

// One reviewed list covering agent_implement's shipped deny set. None of
// these operations is read/repeat-safe: auto-task add/update write scheduler
// definitions, mint creates tasks, command execution has arbitrary effects,
// and pipeline/workflow submission or resumption creates managed work.
const AGENT_IMPLEMENT_TOOL_GRANT_CLASSIFICATIONS: &[(&str, ToolGrantClassification)] = &[
    ("orbit.auto_task.add", GRANTABLE_MUTATION),
    ("orbit.auto_task.mint", GRANTABLE_MUTATION),
    ("orbit.auto_task.update", GRANTABLE_MUTATION),
    ("orbit.command.exec", OPERATOR_MUTATION),
    ("orbit.pipeline.invoke", OPERATOR_MUTATION),
    ("orbit.workflow.run.resume", OPERATOR_MUTATION),
    ("orbit.workflow.ship", OPERATOR_MUTATION),
];

/// Classify a tool grant, defaulting unclassified operations to mutating.
///
/// The governed catalog always wins over grantability: a row without Agent
/// capability cannot be granted, even if it is new or classified as grantable.
/// Callers must separately validate exact canonical registry membership,
/// agent-facing placement and enablement. A classification confers no authority
/// and does not change an activity's deny policy or its capability chokepoint.
pub fn classify_agent_tool_grant(tool_name: &str) -> ToolGrantClassification {
    let mut classification = AGENT_IMPLEMENT_TOOL_GRANT_CLASSIFICATIONS
        .iter()
        .find_map(|(name, classification)| (*name == tool_name).then_some(*classification))
        .unwrap_or(GRANTABLE_MUTATION);
    if catalog::governed_tool(tool_name)
        .is_some_and(|row| !row.allowed.contains(&McpCapability::Agent))
    {
        classification.grantability = ToolGrantability::AlwaysOperatorOnly;
    }
    classification
}
