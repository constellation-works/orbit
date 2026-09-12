pub mod agent;
pub mod auto_task;
pub mod command;
pub mod docs;
pub mod friction;
pub mod operation;
pub mod operation_mode;
pub mod pipeline;
pub mod search;
pub mod semantic;
pub mod task;
pub mod workflow;
pub mod workspace_claim;

use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::reject_unknown_tool_fields;
use orbit_types::identity::{normalize_agent_family_for_model, require_canonical_agent_family};
use orbit_types::tool::{McpToolScope, ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, OrbitTaskScope, ToolContext, ToolRegistry};

pub(super) use orbit_common::protocol::tool_input::{optional_string_alias, required_string};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct OrbitIdentity {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub actor_label: Option<String>,
}

pub fn register(registry: &mut ToolRegistry) {
    // Auto-task definitions are authored by humans through the CLI; the MCP
    // surface carries only what an executing agent needs — reading the
    // definitions and minting one on demand.
    registry.register_inactive(auto_task::add::OrbitAutoTaskAddTool);
    registry.register_mcp(
        auto_task::list::OrbitAutoTaskListTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        auto_task::mint::OrbitAutoTaskMintTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_inactive(auto_task::show::OrbitAutoTaskShowTool);
    registry.register_inactive(auto_task::update::OrbitAutoTaskUpdateTool);
    registry.register_inactive(auto_task::toggle::OrbitAutoTaskToggleTool);
    registry.register_inactive(docs::OrbitDocsListTool);
    registry.register_inactive(docs::OrbitDocsShowTool);
    registry.register_inactive(docs::OrbitDocsAddTool);
    registry.register_inactive(docs::OrbitDocsIndexTool);
    registry.register_inactive(docs::OrbitDocsMigrateTool);
    // Friction schemas and MCP exposure are declared once in the shared
    // operation registry and registered from there.
    friction::register(registry);
    operation_mode::register(registry);
    registry.register_mcp(task::add::OrbitTaskAddTool, McpToolScope::WorkspaceRequired);
    // Attach and read are the two halves of one artifact surface: without a
    // read verb an agent can store a reference it can never inspect again.
    registry.register_mcp(
        task::artifact_get::OrbitTaskArtifactGetTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        task::artifact_put::OrbitTaskArtifactPutTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        task::approve::OrbitTaskApproveTool,
        McpToolScope::WorkspaceRequired,
    );
    // Destructive administration remains reachable through non-MCP surfaces.
    registry.register_inactive(task::delete::OrbitTaskDeleteTool);
    registry.register_inactive(task::lint::OrbitTaskLintTool);
    registry.register_inactive(task::locks::OrbitTaskLocksTool);
    registry.register_inactive(task::locks_reserve::OrbitTaskLocksReserveTool);
    registry.register_inactive(task::locks_release::OrbitTaskLocksReleaseTool);
    // Workspace claims are coordination holds like task locks and remain off
    // the MCP surface.
    registry.register_inactive(workspace_claim::OrbitWorkspaceClaimAcquireTool);
    registry.register_inactive(workspace_claim::OrbitWorkspaceClaimReleaseTool);
    registry.register_inactive(workspace_claim::OrbitWorkspaceClaimShowTool);
    // Agent invocation is workspace-scoped: the admission is made against the
    // checkout that owns it, and Core is where that decision lives.
    registry.register_mcp(agent::OrbitAgentInvokeTool, McpToolScope::WorkspaceRequired);
    // Command execution is workspace-scoped; Core retains its domain and claim
    // validation.
    registry.register_mcp(
        command::OrbitCommandExecTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        task::start::OrbitTaskStartTool,
        McpToolScope::WorkspaceRequired,
    );
    // Task rejection is a human/operator decision — CLI / dashboard only.
    registry.register_inactive(task::reject::OrbitTaskRejectTool);
    registry.register_mcp(
        task::show::OrbitTaskShowTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        task::list::OrbitTaskListTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        task::update::OrbitTaskUpdateTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register(pipeline::invoke::OrbitPipelineInvokeTool);
    registry.register(pipeline::wait::OrbitPipelineWaitTool);
    registry.register_mcp(search::OrbitSearchTool, McpToolScope::WorkspaceRequired);
    registry.register_mcp(
        workflow::OrbitWorkflowShipTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        workflow::OrbitWorkflowRunShowTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        workflow::OrbitWorkflowRunListTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        workflow::OrbitWorkflowRunResumeTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_mcp(
        workflow::OrbitWorkflowRunWorkersTool,
        McpToolScope::WorkspaceRequired,
    );
    registry.register_inactive(semantic::install::OrbitSemanticInstallTool);
    // Destructive semantic-index administration remains on the CLI surface.
    registry.register_inactive(semantic::uninstall::OrbitSemanticUninstallTool);
    registry.register_inactive(semantic::stats::OrbitSemanticStatsTool);
    registry.register_inactive(semantic::index::OrbitSemanticIndexTool);
}

fn trimmed_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn resolve_identity(
    ctx: &ToolContext,
    input: &Value,
) -> Result<OrbitIdentity, OrbitError> {
    let input_agent = optional_string_alias(input, &["agent"])?;
    let input_model = optional_string_alias(input, &["model"])?;
    let context_agent = trimmed_optional(ctx.agent_name.clone());
    let context_model = trimmed_optional(ctx.model_name.clone());
    let context_has_identity = context_agent.is_some() || context_model.is_some();
    let input_has_identity = input_agent
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || input_model
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
    let (agent, model) = if context_has_identity {
        let agent =
            normalize_agent_family_for_model(context_agent.as_deref(), context_model.as_deref())?;
        // Runtime-provided identity is authoritative at the tool boundary. If
        // an agent self-reports a `model` argument, Orbit overwrites it with
        // the canonical family string so downstream persistence compares
        // family identity, not unstable model aliases.
        let model = agent.clone();
        (agent, model)
    } else if input_has_identity {
        (trimmed_optional(input_agent), trimmed_optional(input_model))
    } else {
        (None, None)
    };
    let family = require_canonical_agent_family(agent.as_deref(), model.as_deref())?;
    Ok(OrbitIdentity {
        agent: family.clone(),
        model: family.clone(),
        actor_label: family,
    })
}

pub(super) fn identity_params() -> Vec<ToolParam> {
    vec![
        ToolParam {
            name: "agent".to_string(),
            description:
                "Deprecated compatibility field. Prefer `model` with the agent family (`codex`, `claude`, `gemini`, or `grok`)."
                    .to_string(),
            param_type: "string".to_string(),
            required: false,
        },
        ToolParam {
            name: "model".to_string(),
            description:
                "Preferred provenance field. Pass the canonical agent family (`codex`, `claude`, `gemini`, or `grok`); full model strings are accepted and auto-normalized."
                    .to_string(),
            param_type: "string".to_string(),
            required: false,
        },
    ]
}

pub(super) fn model_identity_params() -> Vec<ToolParam> {
    vec![ToolParam {
        name: "model".to_string(),
        description:
            "Preferred provenance field. Pass the canonical agent family (codex, claude, gemini, or grok); full model strings are accepted and auto-normalized."
                .to_string(),
        param_type: "string".to_string(),
        required: false,
    }]
}

pub(super) fn reject_agent_field(input: &Value, tool_name: &str) -> Result<(), OrbitError> {
    if input
        .as_object()
        .is_some_and(|object| object.contains_key("agent"))
    {
        return Err(OrbitError::InvalidInput(format!(
            "{tool_name} no longer accepts `agent`; use `model` with the agent family for attribution"
        )));
    }
    Ok(())
}

pub(super) fn reject_unknown_tool_arguments(
    input: &Value,
    schema: &ToolSchema,
) -> Result<(), OrbitError> {
    let allowed = schema
        .parameters
        .iter()
        .map(|param| param.name.as_str())
        .collect::<Vec<_>>();
    reject_unknown_tool_fields(input, &allowed)
}

pub(super) fn scored_identity_params() -> Vec<ToolParam> {
    vec![
        ToolParam {
            name: "agent".to_string(),
            description:
                "Deprecated compatibility field. Prefer `model` with the agent family (`codex`, `claude`, `gemini`, or `grok`)."
                    .to_string(),
            param_type: "string".to_string(),
            required: false,
        },
        ToolParam {
            name: "model".to_string(),
            description:
                "Required provenance field. Pass the canonical agent family (`codex`, `claude`, `gemini`, or `grok`), or `human` for human-authored review feedback to opt out of scoreboard scoring. Full model strings are accepted and auto-normalized."
                    .to_string(),
            param_type: "string".to_string(),
            required: true,
        },
    ]
}

pub(super) fn execute_host_action(
    ctx: &ToolContext,
    input: Value,
    action: OrbitBuiltinAction,
) -> Result<Value, OrbitError> {
    let identity = resolve_identity(ctx, &input)?;
    require_orbit_host(ctx)?.execute(
        action,
        input,
        identity.agent,
        identity.model,
        ctx.reservation_owner.clone(),
    )
}

pub(super) fn resolve_workspace_argument(
    ctx: &ToolContext,
    input: &mut Value,
    tool_name: &str,
) -> Result<String, OrbitError> {
    // MCP workspace defaults come from explicit session context, never process cwd.
    // CLI `orbit tool run` binds the runtime through RegisteredRuntimeFactory;
    // individual tools must not repeat workspace resolution.
    let explicit = optional_string_alias(input, &["workspace"])?;
    let session = ctx
        .session_context
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    match (explicit, session) {
        (Some(workspace), Some(session_workspace)) => {
            if workspace != session_workspace {
                tracing::info!(
                    target: "orbit.tools.workspace",
                    tool_name,
                    explicit_workspace = %workspace,
                    session_workspace = %session_workspace,
                    "explicit workspace overrides MCP session context"
                );
            }
            set_input_workspace(input, &workspace)?;
            Ok(workspace)
        }
        (Some(workspace), None) => {
            set_input_workspace(input, &workspace)?;
            Ok(workspace)
        }
        (None, Some(workspace)) => {
            set_input_workspace(input, &workspace)?;
            Ok(workspace)
        }
        (None, None) => Err(OrbitError::InvalidInput(
            "missing `workspace`; pass a registered workspace name, a logical workspace ID \
             (`ws_*`), or an absolute local checkout path, or initialize the MCP session with \
             `_meta.orbit.workspace`"
                .to_string(),
        )),
    }
}

/// Apply the MCP session's configured orchestrator crew to a task-creation
/// call that did not name one [ORB-11313].
///
/// Creation only. Attribution on an existing task is never rewritten from
/// ambient session configuration, so `orbit.task.update` deliberately does
/// not call this and existing records are never backfilled.
///
/// An explicit `orchestrator` on the call always wins; JSON `null` counts as
/// absent, matching how every other optional field reads. The resolved name
/// is written into the input rather than checked here: only the target
/// workspace's crew registry knows which crews exist, and it rejects an
/// unknown name instead of falling back to another crew.
pub(super) fn apply_session_orchestrator_default(ctx: &ToolContext, input: &mut Value) {
    if input
        .get("orchestrator")
        .is_some_and(|value| !value.is_null())
    {
        return;
    }
    let Some(session_orchestrator) = ctx
        .session_context
        .orchestrator
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let Some(object) = input.as_object_mut() else {
        return;
    };
    object.insert(
        "orchestrator".to_string(),
        Value::String(session_orchestrator.to_string()),
    );
}

fn set_input_workspace(input: &mut Value, workspace: &str) -> Result<(), OrbitError> {
    let Some(object) = input.as_object_mut() else {
        return Err(OrbitError::InvalidInput(
            "tool input must be a JSON object".to_string(),
        ));
    };
    object.insert(
        "workspace".to_string(),
        Value::String(workspace.to_string()),
    );
    Ok(())
}

pub(super) fn task_scope(ctx: &ToolContext) -> OrbitTaskScope {
    ctx.orbit_host
        .as_ref()
        .map(|host| host.task_scope())
        .unwrap_or_default()
}

fn require_orbit_host(ctx: &ToolContext) -> Result<&dyn crate::OrbitToolHost, OrbitError> {
    ctx.orbit_host.as_deref().ok_or_else(|| {
        OrbitError::Execution(
            "orbit builtin requires an Orbit runtime host in ToolContext".to_string(),
        )
    })
}

/// Extract an optional string from the first matching key in `keys`.
///
/// Tools accept multiple key names for the same logical field to stay
/// friendly to agents that may use slightly different naming conventions
/// (e.g. `"type"`, `"task_type"`, `"taskType"` all map to the task type
/// parameter). The first non-absent key wins; absence of all keys returns
/// `None`. An explicitly empty value is rejected as an error.
pub(super) fn orbit_id_params(kind: &str) -> Vec<ToolParam> {
    vec![ToolParam {
        name: "id".to_string(),
        description: format!("{kind} ID"),
        param_type: "string".to_string(),
        required: true,
    }]
}

#[cfg(test)]
mod tests;
