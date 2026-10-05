//! Canonical MCP definitions assembled from their owning registries.

use std::sync::{Arc, OnceLock};

use orbit_types::tool::{McpToolDefinition, McpToolDefinitionError};

type CanonicalDefinitions = Result<Arc<Vec<McpToolDefinition>>, McpToolDefinitionError>;

static CANONICAL_DEFINITIONS: OnceLock<Arc<CanonicalDefinitions>> = OnceLock::new();

pub fn canonical_mcp_tool_definitions() -> Result<Vec<McpToolDefinition>, McpToolDefinitionError> {
    cached_canonical_mcp_tool_definitions().map(|definitions| definitions.as_ref().clone())
}

fn cached_canonical_mcp_tool_definitions()
-> Result<Arc<Vec<McpToolDefinition>>, McpToolDefinitionError> {
    CANONICAL_DEFINITIONS
        .get_or_init(|| Arc::new(build_canonical_mcp_tool_definitions()))
        .as_ref()
        .clone()
}

fn build_canonical_mcp_tool_definitions() -> CanonicalDefinitions {
    let mut definitions = orbit_tools::canonical_builtin_mcp_tool_definitions()?;
    definitions.extend(super::discovery::discovery_tool_definitions()?);
    definitions.sort_by(|left, right| left.schema.name.cmp(&right.schema.name));
    orbit_types::tool::validate_mcp_tool_definitions(&definitions)?;
    Ok(Arc::new(definitions))
}

pub fn safe_mcp_tool_names() -> Vec<String> {
    canonical_mcp_tool_definitions()
        .map(|definitions| {
            definitions
                .into_iter()
                .map(|definition| definition.schema.name)
                .collect()
        })
        .unwrap_or_default()
}
