//! Bridging a plugin tool's JSON Schema and the registry's flat
//! [`ToolParam`] list. The registry, `orbit tool show` and the MCP adapter
//! all speak parameters; a manifest speaks JSON Schema.

use orbit_common::protocol::tool_schema::tool_input_schema_for;
use orbit_types::tool::ToolParam;
use serde_json::{Map, Value};

/// Flatten the top-level `properties` of an object schema into parameters.
/// Nested shapes are kept as their outer type; `orbit tool run --input`
/// still accepts the full payload.
pub fn params_from_input_schema(schema: &Value) -> Vec<ToolParam> {
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|(name, property)| ToolParam {
            name: name.clone(),
            description: property
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            param_type: param_type_for(property),
            required: required.iter().any(|entry| entry == name),
        })
        .collect()
}

fn param_type_for(property: &Value) -> String {
    if let Some(kind) = property.get("type").and_then(Value::as_str) {
        return match kind {
            "string" | "integer" | "number" | "boolean" | "array" | "object" => kind.to_string(),
            _ => "string".to_string(),
        };
    }
    let alternatives = property
        .get("anyOf")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    let kind = entry.get("type").and_then(Value::as_str).unwrap_or("");
                    let items = entry
                        .get("items")
                        .and_then(|items| items.get("type"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    (kind, items)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    match alternatives.as_slice() {
        [("array", "string"), ("string", _)] => "string_list".to_string(),
        [("object", _), ("array", "object")] => "object".to_string(),
        [("array", "object"), ("string", _)] => "object_list".to_string(),
        _ => "string".to_string(),
    }
}

/// The canonical object schema for a parameter list, as a manifest
/// `input_schema` value.
pub fn input_schema_from_params(tool_name: &str, params: &[ToolParam]) -> Value {
    let mut schema: Map<String, Value> = tool_input_schema_for(tool_name, params);
    // A manifest schema describes the plugin's contract, not the transport's
    // extra-key policy; the adapter re-applies that when it advertises.
    schema.remove("additionalProperties");
    Value::Object(schema)
}
