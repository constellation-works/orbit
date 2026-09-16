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
