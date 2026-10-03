use orbit_types::tool::ToolSchema;

pub(super) fn tool_schema(name: &str) -> ToolSchema {
    ToolSchema {
        name: name.to_string(),
        description: String::new(),
        parameters: Vec::new(),
        builtin: true,
    }
}
