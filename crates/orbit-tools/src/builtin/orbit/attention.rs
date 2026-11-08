//! Attention scope is fixed by a bound MCP session, never widened by input.
use super::operation::GuardedModes;
use crate::ToolContext;
use orbit_common::{
    OrbitError,
    governance::{attention::AttentionVerb, operation::OperationInput},
};
use serde_json::Value;
impl GuardedModes for AttentionVerb {
    fn preempt(self, ctx: &ToolContext, input: &Value) -> Option<Result<Value, OrbitError>> {
        let session = &ctx.session_context;
        // The reserved hop input is honoured only on a session the federated
        // aggregator opened. The name is self-reported: this keeps ordinary
        // clients on the public page size, it is not an authentication check.
        if input.get("candidate_read").is_some()
            && session.mcp_client_name.as_deref()
                != Some(orbit_common::governance::attention::ATTENTION_AGGREGATOR_CLIENT)
        {
            return Some(Err(
                orbit_common::governance::attention::attention_candidate_read_refusal(),
            ));
        }
        if session
            .transport
            .is_some_and(|transport| transport != orbit_types::tool::McpTransport::Local)
            && (session.workspace_id.is_some() || session.workspace.is_some())
        {
            let widening = input.get("all").and_then(Value::as_bool).unwrap_or(false)
                || input.get("host").is_some()
                || input.get("workspace").is_some_and(|value| {
                    let matches = |value: &Value| {
                        value.as_str().is_some_and(|value| {
                            Some(value) == session.workspace_id.as_deref()
                                || Some(value) == session.workspace.as_deref()
                        })
                    };
                    value.as_array().map_or_else(
                        || !matches(value),
                        |values| values.iter().any(|value| !matches(value)),
                    )
                });
            if widening {
                return Some(Err(
                    orbit_common::governance::attention::attention_scope_refusal(),
                ));
            }
        }
        None
    }
    fn guarded_mode(
        self,
        _ctx: &ToolContext,
        _input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        None
    }
    fn shape_input(self, _ctx: &ToolContext, _input: &mut Value) -> Result<(), OrbitError> {
        Ok(())
    }
    fn schema_extension(self) -> Option<fn(&mut Value)> {
        Some(attention_input_schema)
    }
}

fn attention_input_schema(schema: &mut Value) {
    let kinds = serde_json::json!(orbit_types::tool::AttentionKind::NAMES);
    for field in ["kind", "exclude_kind"] {
        if let Some(property) = schema["properties"][field].as_object_mut() {
            property.remove("anyOf");
            property.insert("type".to_string(), serde_json::json!("array"));
            property.insert(
                "items".to_string(),
                serde_json::json!({ "type": "string", "enum": kinds }),
            );
        }
    }
    schema["properties"]["severity"]["enum"] =
        serde_json::json!(["low", "medium", "high", "critical"]);
    schema["properties"]["limit"]["minimum"] = serde_json::json!(1);
    schema["properties"]["limit"]["maximum"] = serde_json::json!(500);
}
