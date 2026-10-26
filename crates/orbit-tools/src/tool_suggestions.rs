//! Conservative recovery hints for refused tool names. Suggestions never add routes.

use std::collections::BTreeSet;

use orbit_common::governance::authorization::{self, CallerCapabilities, CallerEnvelope};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::tool::{McpCapability, ToolSessionContext, mcp_advertised_tool_name};
use orbit_types::workflow::tool_allowed;

use crate::ToolContext;

/// Build a typed refusal using only the caller's active, enabled, advertised
/// builtin names. The caller supplies that surface; this additionally excludes
/// operator-only operations and tools outside the activity's grants.
///
/// Match read-verb synonyms within a namespace, plus the artifact projection
/// folded into task show. Other names retain the ordinary not-found message.
pub fn unknown_tool_error<'a>(
    name: &str,
    visible_tools: impl IntoIterator<Item = &'a str>,
    context: &ToolContext,
) -> OrbitError {
    let agent = CallerCapabilities::resolve(&CallerEnvelope::mcp_session(&ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Agent]),
        ..Default::default()
    }));
    let mut hints = BTreeSet::new();
    for candidate in visible_tools {
        if !candidate.starts_with("orbit.")
            || (!context.allowed_tools.is_empty()
                && !tool_allowed(candidate, &context.allowed_tools))
            || context
                .tool_deny_policy
                .as_ref()
                .is_some_and(|policy| policy.denies(candidate))
            || authorization::governed_tool(candidate)
                .is_some_and(|operation| authorization::authorize(operation, &agent).is_err())
        {
            continue;
        }
        let Some(namespace) = candidate.strip_suffix(".show") else {
            continue;
        };
        if ["get", "list"].iter().any(|verb| {
            let miss = format!("{namespace}.{verb}");
            name == miss || name == mcp_advertised_tool_name(&miss)
        }) {
            hints.insert(candidate.to_string());
        }
        if candidate == "orbit.task.show"
            && matches!(
                name,
                "orbit.task.artifact.list" | "orbit_task_artifact_list"
            )
        {
            hints.insert(format!("{candidate} with field: \"artifacts\""));
        }
    }
    if hints.len() == 1 {
        OrbitError::not_found_with_guidance(
            NotFoundKind::Tool,
            name,
            format!(
                " (did you mean {}?)",
                hints.into_iter().next().unwrap_or_default()
            ),
        )
    } else {
        OrbitError::not_found(NotFoundKind::Tool, name)
    }
}
