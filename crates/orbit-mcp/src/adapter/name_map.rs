use std::collections::{BTreeMap, HashMap};
use std::fmt;

use orbit_types::tool::{ToolSchema, mcp_advertised_tool_name};
#[cfg(test)]
use rmcp::ErrorData as McpError;
#[cfg(test)]
use serde_json::json;

/// Sanitize an Orbit tool name into the character set MCP clients accept.
///
/// Cursor enforces `[a-zA-Z0-9_]` and VS Code enforces `[a-z0-9_-]`. Replacing
/// `.` with `_` keeps Orbit's existing names within the intersection of both
/// rule sets without renaming any internal canonical identifier.
pub(super) fn sanitize_tool_name(name: &str) -> String {
    mcp_advertised_tool_name(name)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ToolNameCollision {
    pub(super) advertised_name: String,
    pub(super) canonical_names: Vec<String>,
}

impl ToolNameCollision {
    #[cfg(test)]
    pub(super) fn into_mcp_error(self) -> McpError {
        let message = self.to_string();
        McpError::internal_error(
            message,
            Some(json!({
                "code": "tool_name_collision",
                "advertised_name": self.advertised_name,
                "canonical_names": self.canonical_names,
            })),
        )
    }
}

impl fmt::Display for ToolNameCollision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "MCP tool name collision: advertised name '{}' is produced by canonical tools {}; rename one tool before exposing over MCP",
            self.advertised_name,
            self.canonical_names.join(", ")
        )
    }
}

pub(super) fn build_name_map(
    schemas: &[ToolSchema],
) -> Result<HashMap<String, String>, ToolNameCollision> {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for schema in schemas {
        grouped
            .entry(sanitize_tool_name(&schema.name))
            .or_default()
            .push(schema.name.clone());
    }

    let mut map = HashMap::with_capacity(schemas.len());
    for (advertised_name, mut canonical_names) in grouped {
        let has_duplicate = canonical_names.len() > 1;
        canonical_names.sort();
        canonical_names.dedup();
        if has_duplicate {
            return Err(ToolNameCollision {
                advertised_name,
                canonical_names,
            });
        }
        if let Some(canonical_name) = canonical_names.pop() {
            map.insert(advertised_name, canonical_name);
        }
    }
    Ok(map)
}

/// Rewrite the canonical dotted names of advertised tools in `text` to the
/// names `tools/list` advertises.
///
/// A description is written once, in canonical terms, and is shared with
/// `orbit tool list`, where the dotted form is the callable one. An MCP client
/// can only call `orbit_task_show`, so the prose it reads must say that. Only
/// the exact canonical names in `canonical_names` are rewritten, and only as
/// whole names: `_meta.orbit.workspace` and a longer dotted path are left alone.
pub(super) fn advertise_tool_names(text: &str, canonical_names: &[&str]) -> String {
    let is_path_char = |c: char| c.is_alphanumeric() || c == '_' || c == '.';
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    'scan: while let Some(ch) = rest.chars().next() {
        // A name can only start at a word boundary; a path that does not
        // match is copied whole, so a name is never found inside `xorbit.a.b`
        // or `_meta.orbit.workspace`.
        for name in canonical_names {
            if rest.starts_with(name) && name_ends_here(&rest[name.len()..]) {
                out.push_str(&sanitize_tool_name(name));
                rest = &rest[name.len()..];
                continue 'scan;
            }
        }
        let width = if is_path_char(ch) {
            rest.find(|c: char| !is_path_char(c)).unwrap_or(rest.len())
        } else {
            ch.len_utf8()
        };
        out.push_str(&rest[..width]);
        rest = &rest[width..];
    }
    out
}

/// A tool name ends where the next character cannot continue a dotted path;
/// sentence punctuation after the name is not part of it.
fn name_ends_here(after: &str) -> bool {
    let mut chars = after.chars();
    let continues = |c: char| c.is_alphanumeric() || c == '_';
    match chars.next() {
        None => true,
        Some(c) if continues(c) => false,
        Some('.') => !chars.next().is_some_and(continues),
        Some(_) => true,
    }
}

/// [`advertise_tool_names`] over every `description` string in a JSON Schema,
/// wherever it nests.
pub(super) fn advertise_tool_names_in_schema(
    schema: &mut serde_json::Map<String, serde_json::Value>,
    canonical_names: &[&str],
) {
    use serde_json::Value;
    for (key, value) in schema.iter_mut() {
        match value {
            Value::String(text) if key == "description" => {
                *text = advertise_tool_names(text, canonical_names);
            }
            Value::Object(object) => advertise_tool_names_in_schema(object, canonical_names),
            Value::Array(items) => {
                for item in items {
                    if let Value::Object(object) = item {
                        advertise_tool_names_in_schema(object, canonical_names);
                    }
                }
            }
            _ => {}
        }
    }
}
