use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

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
                    "New schedule object: `{ cron: string }` `{ every_minutes: number }`, or `{ deliveries_landed: { branch, threshold, max_wait_minutes, coverage, owner_machine?, max_items?, retries? } }` (owner_machine defaults to this workspace's registered owner machine) (coverage: integrated_qa_v1 or landed_code_review_v1)."
                        .to_string(),
                param_type: "object".to_string(),
                required: false,
            },
            ToolParam {
                name: "template".to_string(),
                description: "Replacement task template object, including optional exact canonical `required_tools` copied to minted tasks.".to_string(),
                param_type: "object".to_string(),
                required: false,
            },
            ToolParam {
                name: "dedupe".to_string(),
                description: "New dedupe policy: `skip_if_open` or `always`.".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
        ];
        ToolSchema {
            name: "orbit.auto_task.update".to_string(),
            description: "Update an existing auto-task definition (present fields only)."
                .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::AutoTaskUpdate)
    }
}
