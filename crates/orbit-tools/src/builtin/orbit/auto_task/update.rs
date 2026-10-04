use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{reject_unknown_tool_fields, required_string};
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::{Value, json};

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitAutoTaskUpdateTool;

impl Tool for OrbitAutoTaskUpdateTool {
    fn schema(&self) -> ToolSchema {
        let parameters = vec![ToolParam {name:"waive_batch".into(),description:"Explicit settled-batch waiver: { batch_id, reason }. Cannot be combined with definition edits; advances no coverage.".into(),param_type:"object".into(),required:false},
            ToolParam {
                name: "name".to_string(),
                description: "Definition name. Required.".to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "description".to_string(),
                description: "New description.".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "schedule".to_string(),
                description:
                    "New schedule object: `{ cron: string }` `{ every_minutes: number }`, or `{ deliveries_landed: { branch, threshold, max_wait_minutes, coverage, owner_machine?, max_items?, retries? } }` (owner_machine defaults to this workspace's registered owner machine) (coverage: landed_code_review_v1)."
                        .to_string(),
                param_type: "object".to_string(),
                required: false,
            },
            ToolParam {
                name: "template".to_string(),
                description: "Replacement task template object, including optional assessed `complexity` (low/medium/hard/xhard) and exact canonical `required_tools` copied to minted tasks.".to_string(),
                param_type: "object".to_string(),
                required: false,
            },
            ToolParam {
                name: "dedupe".to_string(),
                description: "New dedupe policy: `skip_if_open` or `always`.".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "enabled".to_string(),
                description: "Enable (`true`) or disable (`false`) the definition. Disabling is the kill-switch, not a delete.".to_string(),
                param_type: "boolean".to_string(),
                required: false,
            },
            super::super::task::guarded::param(
                "expected_enabled",
                "boolean",
                "Observed enabled state, checked atomically with the `enabled` change; the change is refused when the definition no longer matches. Requires operator authority and `workspace`, and accepts no other edits.",
            ),
            super::super::task::guarded::param(
                "workspace",
                "string",
                "Explicit workspace, required with `expected_enabled`",
            ),
        ];
        ToolSchema {
            name: "orbit.auto_task.update".to_string(),
            description: "Update an existing auto-task definition (present fields only), including enabling or disabling it."
                .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, mut input: Value) -> Result<Value, OrbitError> {
        if input.get("expected_enabled").is_some() {
            // The checked toggle is its own operation: it changes `enabled`
            // and nothing else, so a refused compare leaves no partial edit.
            super::super::domain_control::ensure_operator_leaf(ctx)?;
            reject_unknown_tool_fields(
                &input,
                &["name", "enabled", "expected_enabled", "workspace", "model"],
            )?;
            required_string(&input, &["workspace"], "workspace")?;
            if input.get("enabled").is_none() {
                return Err(OrbitError::InvalidInput(
                    "`expected_enabled` requires the desired `enabled` state".to_string(),
                ));
            }
            input["action"] = json!("toggle");
            input["kind"] = json!("auto_task");
            return super::super::execute_host_action(
                ctx,
                input,
                OrbitBuiltinAction::DesktopAutomation,
            );
        }
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::AutoTaskUpdate)
    }
}
