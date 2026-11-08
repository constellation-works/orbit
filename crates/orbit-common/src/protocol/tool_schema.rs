//! Canonical JSON input schemas for Orbit tools.

use serde_json::{Map, Value, json};

use orbit_types::tool::{ToolParam, ToolSchema};

const TASK_TYPE_ENUM: &[&str] = &["feature", "bug", "refactor", "chore"];
const TASK_STATUS_ENUM: &[&str] = &[
    "proposed",
    "backlog",
    "someday",
    "in-progress",
    "review",
    "done",
    "blocked",
    "rejected",
];
const TASK_COMPLEXITY_ENUM: &[&str] = &["low", "medium", "hard", "xhard"];
/// JSON Schema `pattern` for a provenance `model` value.
///
/// Mirrors `infer_agent_family_from_model` in `orbit-types`: a canonical
/// family (`codex`, `claude`, `gemini`, `grok`, `muse`) or a full model
/// string whose prefix infers one (`gpt-*`/`o1*`/`o3*` → codex,
/// `claude-*`/`opus*`/`sonnet*`/`fable*` → claude, `gemini-*` → gemini,
/// `grok-*`/`grok3*` → grok, `muse-*` → muse). Letter classes make the
/// match case-insensitive, as runtime normalization lowercases first.
/// Unknown values fail this pattern and are also refused at runtime by
/// `require_canonical_agent_family`.
const AGENT_MODEL_PATTERN: &str = concat!(
    r"^(?:",
    r"[Cc][Oo][Dd][Ee][Xx]",
    r"|(?:[Cc][Ll][Aa][Uu][Dd][Ee]|[Gg][Ee][Mm][Ii][Nn][Ii]|[Mm][Uu][Ss][Ee])(?:-.*)?",
    r"|[Gg][Rr][Oo][Kk](?:[-3].*)?",
    r"|(?:[Gg][Pp][Tt]-|[Oo][13]|[Oo][Pp][Uu][Ss]|[Ss][Oo][Nn][Nn][Ee][Tt]|[Ff][Aa][Bb][Ll][Ee]).*",
    r")$"
);

/// Build the canonical JSON input schema for an Orbit tool.
pub fn tool_input_schema(schema: &ToolSchema) -> Map<String, Value> {
    tool_input_schema_for(&schema.name, &schema.parameters)
}

/// Build the canonical JSON input schema from a tool name and parameters.
pub fn tool_input_schema_for(tool_name: &str, params: &[ToolParam]) -> Map<String, Value> {
    let mut properties = Map::new();
    let mut required = Vec::new();

    for param in params {
        let mut property = tool_parameter_schema(&param.param_type);
        if let Some(values) = tool_parameter_enum_values(tool_name, &param.name) {
            property.insert("enum".to_string(), json!(values));
        }
        if let Some(pattern) = tool_parameter_pattern(tool_name, &param.name) {
            property.insert("pattern".to_string(), json!(pattern));
        }
        if !param.description.is_empty() {
            property.insert(
                "description".to_string(),
                Value::String(param.description.clone()),
            );
        }
        properties.insert(param.name.clone(), Value::Object(property));
        if param.required {
            required.push(Value::String(param.name.clone()));
        }
    }

    let mut schema = Map::new();
    schema.insert("type".to_string(), Value::String("object".to_string()));
    schema.insert("properties".to_string(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert("required".to_string(), Value::Array(required));
    }
    schema.insert(
        "additionalProperties".to_string(),
        Value::Bool(tool_arguments_allow_additional_properties(tool_name)),
    );
    schema
}

/// Whether the advertised argument object accepts undeclared keys.
///
/// Registered task tools refuse extras at runtime; their schemas match that
/// contract. Other tools keep `additionalProperties: true` until
/// they grow the same check. Transport wrappers (`_meta`, `workspace`) are
/// not modeled as additional argument properties.
pub fn tool_arguments_allow_additional_properties(tool_name: &str) -> bool {
    !tool_name.starts_with("orbit.task.")
}

/// Build the canonical JSON-Schema fragment for one tool parameter.
pub fn tool_parameter_schema(param_type: &str) -> Map<String, Value> {
    let mut schema = Map::new();
    match param_type.trim().to_ascii_lowercase().as_str() {
        "string" | "str" | "text" | "enum" | "path" | "url" => {
            schema.insert("type".to_string(), Value::String("string".to_string()));
        }
        "integer" | "int" | "u8" | "u16" | "u32" | "u64" | "usize" | "i8" | "i16" | "i32"
        | "i64" | "isize" => {
            schema.insert("type".to_string(), Value::String("integer".to_string()));
        }
        "number" | "float" | "f32" | "f64" => {
            schema.insert("type".to_string(), Value::String("number".to_string()));
        }
        "boolean" | "bool" => {
            schema.insert("type".to_string(), Value::String("boolean".to_string()));
        }
        "string_list" | "string[]" | "strings" => {
            // MCP advertises arrays only. Runtime still accepts one string or a
            // comma-separated string so the CLI and existing callers keep working.
            schema.insert("type".to_string(), Value::String("array".to_string()));
            schema.insert("items".to_string(), json!({ "type": "string" }));
        }
        "array" | "list" => {
            schema.insert("type".to_string(), Value::String("array".to_string()));
        }
        "object" | "map" | "json" => {
            schema.insert(
                "anyOf".to_string(),
                json!([
                    { "type": "object" },
                    { "type": "array", "items": { "type": "object" } },
                ]),
            );
        }
        "object_array" => {
            schema.insert("type".to_string(), Value::String("array".to_string()));
            schema.insert("items".to_string(), json!({ "type": "object" }));
        }
        "object_list" | "object[]" | "objects" => {
            schema.insert(
                "anyOf".to_string(),
                json!([
                    { "type": "array", "items": { "type": "object" } },
                    { "type": "string" },
                ]),
            );
        }
        _ => {
            tracing::warn!(
                target: "orbit.common.tool_schema",
                param_type,
                "unknown ToolParam type degrading to string"
            );
            schema.insert("type".to_string(), Value::String("string".to_string()));
        }
    }
    schema
}

/// Canonical enum metadata shared by every Orbit tool transport.
pub fn tool_parameter_enum_values(
    tool_name: &str,
    param_name: &str,
) -> Option<&'static [&'static str]> {
    match (tool_name, param_name) {
        ("orbit.task.add" | "orbit.task.update", "type") => Some(TASK_TYPE_ENUM),
        ("orbit.task.add" | "orbit.task.update", "status") => Some(TASK_STATUS_ENUM),
        ("orbit.task.add" | "orbit.task.update", "complexity") => Some(TASK_COMPLEXITY_ENUM),
        // No MCP tool advertises `model`. A parameter that is still named
        // `model` (a CLI filter, or the unadvertised identity key) accepts
        // families and full model strings, so it has no finite enum.
        (_, "model") => None,
        _ => None,
    }
}

/// JSON Schema `pattern` for a tool parameter, when its accepted set is
/// a recognizable prefix family rather than a finite enum.
pub fn tool_parameter_pattern(tool_name: &str, param_name: &str) -> Option<&'static str> {
    let _ = tool_name;
    match param_name {
        "model" => Some(AGENT_MODEL_PATTERN),
        _ => None,
    }
}
