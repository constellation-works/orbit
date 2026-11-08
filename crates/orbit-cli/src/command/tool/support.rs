use orbit_core::adapter::command::ToolInfo;
use orbit_types::tool::ToolParam;

pub(super) fn tool_status(tool: &ToolInfo) -> &'static str {
    if !tool.active {
        "inactive"
    } else if !tool.enabled {
        "disabled"
    } else {
        "active"
    }
}

pub(super) fn format_required_tool_input_summary(parameters: &[ToolParam]) -> String {
    let required_inputs = parameters
        .iter()
        .filter(|param| param.required)
        .map(format_tool_param_signature)
        .collect::<Vec<_>>();

    if required_inputs.is_empty() {
        "-".to_string()
    } else {
        required_inputs.join(", ")
    }
}

fn format_tool_param_signature(param: &ToolParam) -> String {
    if param.param_type.trim().is_empty() {
        param.name.clone()
    } else {
        format!("{}:{}", param.name, display_param_type(&param.param_type))
    }
}

fn display_param_type(param_type: &str) -> &str {
    match param_type {
        "string_list" => "string[]",
        "object_array" => "object[]",
        other => other,
    }
}
