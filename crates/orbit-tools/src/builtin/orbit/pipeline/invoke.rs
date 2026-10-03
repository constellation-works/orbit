use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::{Value, json};

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitPipelineInvokeTool;

/// Whether host-owned invocation context records a sanctioned child admission.
/// Caller tool arguments are never consulted here; the application validates
/// the admission fields and parent identity before submitting the child.
pub fn has_pipeline_child_admission(ctx: &ToolContext) -> bool {
    ctx.reservation_owner
        .as_ref()
        .and_then(|owner| owner.owner_metadata_json.as_deref())
        .and_then(|metadata| serde_json::from_str::<Value>(metadata).ok())
        .is_some_and(|metadata| {
            metadata
                .get("pipeline_child_admission")
                .is_some_and(Value::is_object)
        })
}

impl Tool for OrbitPipelineInvokeTool {
    fn schema(&self) -> ToolSchema {
        let mut parameters = vec![
            ToolParam {
                name: "job_name".to_string(),
                description: "Registered v2 job name to invoke.".to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "input".to_string(),
                description: "Required unless default_input:true: JSON object forwarded as the run input payload.".to_string(),
                param_type: "object".to_string(),
                required: false,
            },
            ToolParam {
                name: "priority".to_string(),
                description: "Optional queue priority (`low`, `medium`, `high`, or `critical`)."
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
        ];
        parameters.extend([
            super::super::task::guarded::param("workspace", "string", "Explicit workspace required for default-input catalog submission"),
            super::super::task::guarded::param("default_input", "boolean", "True submits an enabled, no-input catalog job with its defaults. Requires trusted operator authority and forbids managed-run invocation, input and priority. After a lost reply inspect runs; never automatically replay."),
        ]);
        parameters.extend(super::super::identity_params());

        ToolSchema {
            name: "orbit.pipeline.invoke".to_string(),
            description: "Submit a durable pipeline run and return immediately with its run ID. Public submissions require an operator; managed runs may invoke only a host-authorized child pipeline admission."
                .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn input_schema(&self) -> Option<Value> {
        let mut schema = Value::Object(orbit_common::protocol::tool_schema::tool_input_schema(
            &self.schema(),
        ));
        schema["allOf"] = json!([{"if":{"required":["default_input"],"properties":{"default_input":{"const":true}}},"then":{"required":["workspace"],"propertyNames":{"enum":["workspace","job_name","default_input","model"]}},"else":{"required":["input"]}}]);
        Some(schema)
    }

    fn execute(&self, ctx: &ToolContext, mut input: Value) -> Result<Value, OrbitError> {
        if let Some(default_input) = input.get("default_input") {
            let enabled = default_input.as_bool().ok_or_else(|| {
                OrbitError::InvalidInput("default_input must be a boolean".into())
            })?;
            if enabled {
                if ctx
                    .orbit_host
                    .as_ref()
                    .is_some_and(|host| host.task_scope().run_id.is_some())
                {
                    return Err(OrbitError::CapabilityDenied(
                        "managed runs cannot submit catalog jobs".into(),
                    ));
                }
                orbit_common::protocol::tool_input::reject_unknown_tool_fields(
                    &input,
                    &["workspace", "job_name", "default_input", "model"],
                )?;
                orbit_common::protocol::tool_input::required_string(
                    &input,
                    &["workspace"],
                    "workspace",
                )?;
                let name = orbit_common::protocol::tool_input::required_string(
                    &input,
                    &["job_name"],
                    "job_name",
                )?;
                let model = input.get("model").cloned();
                input = json!({"workspace":input["workspace"], "name":name, "action":"run", "kind":"job"});
                if let Some(model) = model {
                    input["model"] = model;
                }
                return super::super::execute_host_action(
                    ctx,
                    input,
                    OrbitBuiltinAction::DesktopAutomation,
                );
            }
        }
        if ctx
            .orbit_host
            .as_ref()
            .is_some_and(|host| host.task_scope().run_id.is_some())
            && !has_pipeline_child_admission(ctx)
        {
            return Err(OrbitError::CapabilityDenied(
                "managed runs may invoke only a host-authorized child pipeline admission".into(),
            ));
        }
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::PipelineInvoke)
    }
}
