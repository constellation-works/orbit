use orbit_types::tool::ToolParam;

use super::super::schema::{input_schema_from_params, params_from_input_schema};

fn param(name: &str, param_type: &str, required: bool) -> ToolParam {
    ToolParam {
        name: name.to_string(),
        description: format!("{name} description"),
        param_type: param_type.to_string(),
        required,
    }
}

#[test]
fn parameter_types_round_trip_through_json_schema() {
    let params = vec![
        param("repository", "string", true),
        param("limit", "integer", false),
        param("hybrid", "boolean", false),
        param("task_ids", "string_list", false),
        param("delivery", "object", false),
        param("hits", "array", false),
        param("snapshots", "object_list", false),
        param("ratio", "number", false),
    ];
    let schema = input_schema_from_params("demo.tool", &params);
    assert!(schema.get("additionalProperties").is_none());
    let mut recovered = params_from_input_schema(&schema);
    recovered.sort_by(|left, right| left.name.cmp(&right.name));
    let mut expected = params.clone();
    expected.sort_by(|left, right| left.name.cmp(&right.name));
    assert_eq!(recovered, expected);
}

#[test]
fn unknown_shapes_degrade_to_string_without_required_flags() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "weird": { "oneOf": [{ "type": "null" }] } }
    });
    let params = params_from_input_schema(&schema);
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].param_type, "string");
    assert!(!params[0].required);
    assert!(params_from_input_schema(&serde_json::json!({ "type": "object" })).is_empty());
}
