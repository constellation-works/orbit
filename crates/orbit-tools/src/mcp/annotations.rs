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
//! Rows name operations; a tool family advertises their combination.
//!
//! The hints are advisory UX for the client. They never authorize a call.

use orbit_types::tool::{McpToolAnnotations, ToolFamily, ToolSchema, tool_family};

use crate::ToolExecutionKind;

/// Hints for one built-in tool, by canonical name.
fn builtin_annotations(canonical_name: &str) -> Option<McpToolAnnotations> {
    use McpToolAnnotations as A;
    Some(match canonical_name {
        // Observation of Orbit's own state.
        "orbit.agents.ci_failures"
        | "orbit.agents.scorecard"
        | "orbit.agents.show"
        | "orbit.auto_task.list"
        | "orbit.auto_task.show"
        | "orbit.automation.explain"
        | "orbit.attention"
        | "orbit.codebase.analytics"
        | "orbit.codebase.related"
        | "orbit.codebase.show"
        | "orbit.decision.list"
        | "orbit.decision.show"
        | "orbit.drain.claims"
        | "orbit.friction.show"
        | "orbit.friction.stats"
        | "orbit.friction.tags"
        | "orbit.memory.show"
        | "orbit.notice.list"
        | "orbit.notice.show"
        | "orbit.evolution.list"
        | "orbit.evolution.show"
        | "orbit.resident.digest"
        | "orbit.resident.list"
        | "orbit.resident.show"
        | "orbit.orchestrator.scorecard"
        | "orbit.orchestrator.show"
        | "orbit.search"
        | "orbit.task.artifact.get"
        | "orbit.task.eligible"
        | "orbit.task.list"
        | "orbit.task.show"
        | "orbit.workflow.run.list"
        | "orbit.workflow.run.observe"
        | "orbit.workflow.run.show" => A::READ_ONLY,

        // Creation: adds a record, never removes or overwrites one, and a
        // repeated call adds another.
        "orbit.auto_task.add"
        | "orbit.auto_task.mint"
        | "orbit.decision.raise"
        | "orbit.friction.add"
        | "orbit.evolution.add"
        | "orbit.memory.propose"
        | "orbit.task.add" => A::additive(false),

        // Edits that set the fields given and touch nothing else.
        "orbit.auto_task.update" => A::additive(true),

        // A repeated close returns the notice unchanged.
        "orbit.notice.close" => A::additive(true),

        // A task edit can append a note or comment, so a repeat is not a no-op.
        "orbit.task.update" | "orbit.resident.checkpoint" | "orbit.resident.relay" => {
            A::additive(false)
        }

        // Removes or replaces existing data.
        // A friction edit with `rehome_to` moves the record and resolves the
        // original, so a repeat is refused rather than a no-op.
        "orbit.friction.update" => A::destructive(false),
        // A post whose key an open notice holds replaces that notice's
        // content; repeating it matches the same notice and changes nothing.
        "orbit.notice.post" => A::destructive(true),
        // A narrowing removes authority; repeating a grant returns the open
        // request or finds the rights already held.
        "orbit.resident.grant" => A::destructive(true),
        // Pause, stop and takeover stop a resident firing; each call writes
        // another history entry.
        "orbit.resident.control" => A::destructive(false),
        "orbit.task.artifact.put" => A::destructive(true),
        // Replaces a memory space's files with a new revision; each call
        // appends another revision.
        "orbit.memory.write" | "orbit.memory.proposal.accept" => A::destructive(false),
        "orbit.memory.proposal.reject" => A::additive(false),
        // Listing can withdraw newly expired proposals; a repeat changes none.
        "orbit.memory.proposals" => A::destructive(true),
        "orbit.task.review_reset" => A::destructive(false),
        // Supersedes a recorded escalation; with `discard_candidate` it also
        // closes the published pull request.
        "orbit.task.requeue" => A::destructive(false),
        "orbit.task.recover_claim" => A::destructive(false),
        "orbit.task.revoke_handoff" => A::destructive(false),
        // A requeue supersedes a recorded escalation; a second decision on
        // the same block episode is refused rather than a no-op.
        "orbit.task.block_triage" => A::destructive(false),

        // Starts work outside Orbit's own state: an agent, a workflow run's
        // agents, or an arbitrary process.
        "orbit.task.retry_handoff"
        | "orbit.task.reconcile_review"
        | "orbit.pipeline.invoke"
        | "orbit.workflow.auto"
        | "orbit.routine.control"
        | "orbit.command.exec"
        | "orbit.workflow.land"
        | "orbit.workflow.run.resume"
        | "orbit.workflow.ship" => A::OPEN_WORLD,

        _ => return None,
    })
}

/// The `annotations` a tool advertises over MCP. A tool family combines its
/// operations' hints so that no action is understated: it is read-only only
/// when every action is, idempotent only when every action is, and
/// destructive or open-world when any action is.
/// `operation_kind` gives the execution kind of a family's operation, for an
/// operation without its own row.
pub(crate) fn mcp_annotations(
    schema: &ToolSchema,
    execution_kind: ToolExecutionKind,
    operation_kind: impl Fn(&str) -> Option<ToolExecutionKind>,
) -> McpToolAnnotations {
    if schema.builtin
        && let Some(family) = tool_family(&schema.name)
    {
        return family_annotations(family, operation_kind);
    }
    operation_annotations(schema.builtin, &schema.name, execution_kind)
}

fn operation_annotations(
    builtin: bool,
    name: &str,
    execution_kind: ToolExecutionKind,
) -> McpToolAnnotations {
    if builtin && let Some(annotations) = builtin_annotations(name) {
        return annotations;
    }
    McpToolAnnotations {
        read_only: Some(execution_kind == ToolExecutionKind::ReadOnly),
        ..McpToolAnnotations::default()
    }
}

fn family_annotations(
    family: &ToolFamily,
    operation_kind: impl Fn(&str) -> Option<ToolExecutionKind>,
) -> McpToolAnnotations {
    let hints = family
        .actions
        .iter()
        .map(|action| {
            operation_kind(action.operation).map_or_else(McpToolAnnotations::default, |kind| {
                operation_annotations(true, action.operation, kind)
            })
        })
        .collect::<Vec<_>>();
    // An unstated hint takes the MCP default: not read-only, destructive,
    // not idempotent, open-world.
    let read_only = hints.iter().all(|hint| hint.read_only == Some(true));
    if read_only {
        return McpToolAnnotations {
            open_world: Some(hints.iter().any(|hint| hint.open_world != Some(false))),
            ..McpToolAnnotations::READ_ONLY
        };
    }
    let writes = hints.iter().filter(|hint| hint.read_only != Some(true));
    McpToolAnnotations {
        read_only: Some(false),
        destructive: Some(writes.clone().any(|hint| hint.destructive != Some(false))),
        idempotent: Some(writes.clone().all(|hint| hint.idempotent == Some(true))),
        open_world: Some(hints.iter().any(|hint| hint.open_world != Some(false))),
    }
}
