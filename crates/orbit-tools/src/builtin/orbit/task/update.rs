use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitTaskUpdateTool;

impl Tool for OrbitTaskUpdateTool {
    fn schema(&self) -> ToolSchema {
        let mut parameters = super::super::orbit_id_params("task");
        parameters.extend([
            ToolParam {
                name: "title".to_string(),
                description: "New task title".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "description".to_string(),
                description: "New task description (empty string clears)".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "acceptance_criteria".to_string(),
                description: "New acceptance criteria as an array of strings or a single string"
                    .to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "dependencies".to_string(),
                description: "Replacement dependency task IDs as a string or array of strings"
                    .to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "relations".to_string(),
                description: "Replacement typed task relations as an array of {type, target} objects"
                    .to_string(),
                param_type: "array".to_string(),
                required: false,
            },
            ToolParam {
                name: "tags".to_string(),
                description: "Replacement task tags as a string or array of strings".to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "plan".to_string(),
                description: "Replacement task plan text (empty string clears). May be supplied on the same write that transitions to in-progress when a plan is required.".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "status".to_string(),
                description: "New task status. `backlog` on a proposed task is the approval transition and cannot be combined with field edits (only `note` and `comment`). `in-progress` from a pickup state is the start transition; `plan` and `crew` may be supplied on that write. Other status changes are ordinary governed updates and may include field edits.".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "note".to_string(),
                description: "Optional lifecycle note for the guarded approval (proposed → backlog) or start (pickup → in-progress) transition; rejected on any other update"
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "priority".to_string(),
                description: "New dispatch priority (low, medium, high, or critical)".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "complexity".to_string(),
                description: "Optional task complexity level (low, medium, or hard)".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "type".to_string(),
                description: "New task type".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "source_task_id".to_string(),
                description: "For bug tasks: originating task ID that introduced the defect (empty string clears)"
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "planned_by".to_string(),
                description: "Explicit planning attribution label (empty string clears)"
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "implemented_by".to_string(),
                description: "Explicit implementation attribution label (empty string clears)"
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "execution_summary".to_string(),
                description: "Replacement execution summary text".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "comment".to_string(),
                description: "Task comment to append".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "pr_status".to_string(),
                description: "PR review status (e.g. approve, request-changes; empty string clears)"
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "job_run_id".to_string(),
                description: "Job run ID to associate with the task (empty string clears)"
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "crew".to_string(),
                description: "Named crew to use when running this task (empty string clears)"
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "orchestrator".to_string(),
                description: "Named crew responsible for orchestration attribution (empty string clears; mutable only before execution)".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "context_files".to_string(),
                description:
                    "Task context selectors as a comma-separated string or array of strings. Add entries ONLY for existing files, directories, or symbols expected to be modified or deleted by the task. Do not add background-reading entries or files referenced only for context. Prefer canonical selectors: `file:path`, `dir:path`, or `symbol:path#name:kind`. Legacy raw paths are accepted and upgraded automatically. Existence checks verify the filesystem anchor only; a `symbol:` name and kind are not looked up."
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
                name: "context".to_string(),
                description:
                    "Legacy alias for `context_files`. Add entries ONLY for existing files, directories, or symbols expected to be modified or deleted by the task. Do not add background-reading entries or files that are only relevant background context. Prefer canonical selectors: `file:path`, `dir:path`, or `symbol:path#name:kind`. Existence checks verify the filesystem anchor only; a `symbol:` name and kind are not looked up."
                        .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
        ]);
        parameters.extend(super::super::model_identity_params());

        ToolSchema {
            name: "orbit.task.update".to_string(),
            description: "Update an Orbit task and return the fresh task JSON. Field edits may accompany a status change, except `status: backlog` on a proposed task (approval), which accepts only `note` and `comment`. Starting with `status: in-progress` may include `plan` and `crew` on the same write.".to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::reject_agent_field(&input, "orbit.task.update")?;
        if ["required_tools", "requiredTools", "required-tool"]
            .iter()
            .any(|field| input.get(*field).is_some())
        {
            return Err(OrbitError::InvalidInput(
                "orbit.task.update does not accept `required_tools`; task tool requirements are immutable after creation"
                    .to_string(),
            ));
        }
        if input.get("force").is_some() {
            return Err(OrbitError::InvalidInput(
                "orbit.task.update does not accept `force`; lifecycle transitions are enforced for agents, and the override is a human CLI action"
                    .to_string(),
            ));
        }
        if input.get("artifacts").is_some() {
            return Err(OrbitError::InvalidInput(
                "orbit.task.update does not accept inline artifacts; use orbit.task.artifact.put"
                    .to_string(),
            ));
        }
        super::super::reject_unknown_tool_arguments(&input, &self.schema())?;
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::TaskUpdate)
    }
}
