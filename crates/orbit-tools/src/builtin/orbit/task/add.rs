use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{reject_retired_task_add_input_fields, required_string};
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitTaskAddTool;

impl Tool for OrbitTaskAddTool {
    fn schema(&self) -> ToolSchema {
        let mut parameters = vec![
            ToolParam {
                name: "title".to_string(),
                description: "Task title".to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "description".to_string(),
                description: "Task description markdown".to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            // `workspace` is the binding key for ~/.orbit/tasks/workspaces/<id>/
            // home-store projection; ambient MCP session context may supply this field,
            // but process cwd must never be used as the fallback.
            ToolParam {
                name: "workspace".to_string(),
                description:
                    "Workspace selector: a registered workspace name, a logical workspace ID \
                     (`ws_*`), or an absolute path to a local checkout (a linked Git worktree \
                     resolves to its registered checkout). Falls back to the MCP session's \
                     `_meta.orbit.workspace` when omitted; never inferred from process cwd."
                        .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "acceptance_criteria".to_string(),
                description: "Optional acceptance criteria as a string or array of strings"
                    .to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "tags".to_string(),
                description: "Optional tags as a string or array of strings".to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "required_tools".to_string(),
                description: "Optional exact canonical tool names the task adds to its agent activity baseline, as a string or array of strings".to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "context_files".to_string(),
                description:
                    "Optional task context selectors as a comma-separated string or array of strings. Add entries ONLY for existing files, directories, or symbols expected to be modified or deleted by the task. Do not add background-reading entries or files referenced only for context. Prefer canonical selectors: `file:`, `dir:`, or `symbol:path#name:kind`. Legacy raw paths are accepted and upgraded automatically. Existence checks verify the filesystem anchor only; a `symbol:` name and kind are not looked up."
                        .to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "allow_missing_context".to_string(),
                description: "Optional. Set true to accept `context_files` selectors whose target does not exist yet, for work that creates the file. Missing selectors are rejected by default."
                    .to_string(),
                param_type: "boolean".to_string(),
                required: false,
            },
            ToolParam {
                name: "priority".to_string(),
                description: "Optional priority level".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "complexity".to_string(),
                description: "Task complexity level (low, medium, or hard)".to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "type".to_string(),
                description: "Optional task type".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "relations".to_string(),
                description:
                    "Optional typed task relations as an array of {type, target} objects"
                        .to_string(),
                param_type: "array".to_string(),
                required: false,
            },
            ToolParam {
                name: "crew".to_string(),
                description: "Optional named crew to use when running this task".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "orchestrator".to_string(),
                description: "Optional named crew responsible for orchestration attribution; does not select execution. Defaults to the MCP session's `orbit mcp serve --orchestrator` crew when omitted".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
        ];
        parameters.extend(super::super::model_identity_params());

        ToolSchema {
            name: "orbit.task.add".to_string(),
            description: "Create an Orbit task and return the created task JSON".to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, mut input: Value) -> Result<Value, OrbitError> {
        super::super::reject_agent_field(&input, "orbit.task.add")?;
        reject_retired_task_add_input_fields(&input)?;
        super::super::reject_unknown_tool_arguments(&input, &self.schema())?;
        required_string(&input, &["title"], "title")?;
        required_string(&input, &["description"], "description")?;
        required_string(&input, &["complexity"], "complexity")?;
        super::super::resolve_workspace_argument(ctx, &mut input, "orbit.task.add")?;
        super::super::apply_session_orchestrator_default(ctx, &mut input);

        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::TaskAdd)
    }
}
