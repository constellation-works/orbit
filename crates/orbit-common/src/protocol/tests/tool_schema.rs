use orbit_types::tool::ToolParam;

use crate::protocol::tool_schema::{
    tool_arguments_allow_additional_properties, tool_input_schema_for,
};

fn param(name: &str) -> ToolParam {
    ToolParam {
        name: name.to_string(),
        description: String::new(),
        param_type: "string".to_string(),
        required: false,
    }
}

#[test]
fn task_mutation_argument_schemas_forbid_additional_properties() {
    for tool_name in ["orbit.task.add", "orbit.task.update"] {
        assert!(
            !tool_arguments_allow_additional_properties(tool_name),
            "{tool_name} arguments must be closed"
        );
        let schema = tool_input_schema_for(tool_name, &[param("id")]);
        assert_eq!(
            schema
                .get("additionalProperties")
                .and_then(|value| value.as_bool()),
            Some(false),
            "{tool_name}"
        );
    }
}

#[test]
fn other_tool_argument_schemas_still_allow_additional_properties() {
    assert!(tool_arguments_allow_additional_properties(
        "orbit.task.show"
    ));
    let schema = tool_input_schema_for("orbit.task.list", &[param("status")]);
    assert_eq!(
        schema
            .get("additionalProperties")
            .and_then(|value| value.as_bool()),
        Some(true)
    );
}
