use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitAutoTaskAddTool;

impl Tool for OrbitAutoTaskAddTool {
    fn schema(&self) -> ToolSchema {
        let parameters = vec![
            ToolParam {
                name: "name".to_string(),
                description: "Unique definition name (lowercase alphanumeric, `-`/`_`, starts alphanumeric). Required.".to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "description".to_string(),
                description: "Human description of the recurring chore.".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "schedule".to_string(),
                description: "Schedule object: exactly one of `{ cron: string }` (5-field cron) `{ every_minutes: number }`, or `{ deliveries_landed: { branch, threshold, max_wait_minutes, coverage, owner_machine?, max_items?, retries? } }` (coverage: integrated_qa_v1 or landed_code_review_v1; owner_machine defaults to this workspace's registered owner machine). Required.".to_string(),
                param_type: "object".to_string(),
                required: true,
            },
            ToolParam {
                name: "template".to_string(),
                description: "Task template: `{ title, description?, acceptance_criteria?, task_type?, tags?, required_tools?, priority?, crew?, status? }`. `required_tools` is an exact canonical-name list copied to every minted task. Provider-neutral — no turn knobs. Required.".to_string(),
                param_type: "object".to_string(),
                required: true,
            },
            ToolParam {
                name: "dedupe".to_string(),
                description: "Dedupe policy: `skip_if_open` (default) or `always`.".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
        ];
        ToolSchema {
            name: "orbit.auto_task.add".to_string(),
            description: "Create a recurring auto-task definition. The generic scheduler mints a task from its template on each due slot.".to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::AutoTaskAdd)
    }
}
