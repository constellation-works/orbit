//! MCP behavior hints for the built-in tool surface.
//!
//! `tools/list` advertises each tool's `annotations` so a client can decide
//! what to auto-approve. The read-only fact comes from the tool's own
//! [`ToolExecutionKind`]; the remaining hints (destructive, idempotent,
//! open-world) are properties of the operation that no trait method carries,
//! so they are declared once here, by canonical tool name. A built-in without
//! a row, and every plugin tool, advertises only what its execution kind
//! proves: a client applies the MCP defaults (destructive, open-world) for
//! anything left unsaid, which is the safe reading.
//!
//! The hints are advisory UX for the client. They never authorize a call.

use orbit_types::tool::{McpToolAnnotations, ToolSchema};

use crate::ToolExecutionKind;

/// Hints for one built-in tool, by canonical name.
fn builtin_annotations(canonical_name: &str) -> Option<McpToolAnnotations> {
    use McpToolAnnotations as A;
    Some(match canonical_name {
        // Observation of Orbit's own state.
        "orbit.auto_task.list"
        | "orbit.auto_task.show"
        | "orbit.drain.claims"
        | "orbit.drain.probe"
        | "orbit.drain.receipt.lookup"
        | "orbit.friction.list"
        | "orbit.friction.show"
        | "orbit.friction.stats"
        | "orbit.friction.tags"
        | "orbit.search"
        | "orbit.task.artifact.get"
        | "orbit.task.list"
        | "orbit.task.show"
        | "orbit.workflow.run.delivery"
        | "orbit.workflow.run.list"
        | "orbit.workflow.run.show" => A::READ_ONLY,

        // Creation: adds a record, never removes or overwrites one, and a
        // repeated call adds another.
        "orbit.auto_task.add"
        | "orbit.auto_task.mint"
        | "orbit.friction.add"
        | "orbit.task.add"
        | "orbit.task.pull" => A::additive(false),

        // Edits that set the fields given and touch nothing else.
        "orbit.auto_task.toggle"
        | "orbit.auto_task.update"
        | "orbit.drain.claim.bind"
        | "orbit.drain.claim.settle"
        | "orbit.friction.update"
        | "orbit.workflow.run.workers" => A::additive(true),

        // A task edit can append a note or comment, so a repeat is not a no-op.
        "orbit.task.update" => A::additive(false),

        // Removes or replaces existing data.
        "orbit.auto_task.delete" | "orbit.friction.rehome" => A::destructive(false),
        "orbit.task.artifact.put" => A::destructive(true),

        // Starts work outside Orbit's own state: an agent, a workflow run's
        // agents, or an arbitrary process.
        "orbit.agent.invoke"
        | "orbit.command.exec"
        | "orbit.workflow.run.resume"
        | "orbit.workflow.ship" => A::OPEN_WORLD,

        _ => return None,
    })
}

/// The `annotations` a tool advertises over MCP.
pub(crate) fn mcp_annotations(
    schema: &ToolSchema,
    execution_kind: ToolExecutionKind,
) -> McpToolAnnotations {
    if schema.builtin
        && let Some(annotations) = builtin_annotations(&schema.name)
    {
        return annotations;
    }
    McpToolAnnotations {
        read_only: Some(execution_kind == ToolExecutionKind::ReadOnly),
        ..McpToolAnnotations::default()
    }
}
