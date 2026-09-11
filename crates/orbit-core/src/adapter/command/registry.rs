//! Tool registry CRUD: listing, inspection, add/remove, enable/disable, and
//! the registry health check behind `orbit tool doctor`.

use std::path::Path;

use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::record::OrbitEvent;
use orbit_types::tool::{McpToolDefinition, StoredTool, ToolParam};

use crate::OrbitRuntime;

#[derive(Debug, Clone)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub active: bool,
    pub builtin: bool,
    pub parameters: Vec<orbit_types::tool::ToolParam>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoctorStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct DoctorResult {
    pub tool_name: String,
    pub status: DoctorStatus,
    pub message: String,
}

impl OrbitRuntime {
    /// Return runtime-enabled MCP definitions with their workspace scope.
    pub fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        let stored_tools = self.stores().tools().list_tools()?;
        let mut definitions = self
            .tool_registry()
            .mcp_tool_definitions()
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        definitions.retain(|definition| {
            stored_tools
                .iter()
                .find(|stored| stored.name == definition.schema.name)
                .is_none_or(|stored| stored.enabled)
        });
        Ok(definitions)
    }

    pub fn list_tools(&self) -> Result<Vec<ToolInfo>, OrbitError> {
        self.list_tools_with_inactive(false)
    }

    pub fn list_all_tools(&self) -> Result<Vec<ToolInfo>, OrbitError> {
        self.list_tools_with_inactive(true)
    }

    fn list_tools_with_inactive(
        &self,
        include_inactive: bool,
    ) -> Result<Vec<ToolInfo>, OrbitError> {
        let registry_schemas = if include_inactive {
            self.tool_registry().all_schemas()
        } else {
            self.tool_registry().schemas()
        };
        let stored_tools = self.stores().tools().list_tools()?;

        let mut tools: Vec<ToolInfo> = registry_schemas
            .into_iter()
            .map(|schema| {
                let stored = stored_tools.iter().find(|s| s.name == schema.name);
                let enabled = stored.is_none_or(|s| s.enabled);
                let active = self.tool_registry().is_active(&schema.name);
                ToolInfo {
                    name: schema.name.clone(),
                    description: schema.description.clone(),
                    enabled,
                    active,
                    builtin: schema.builtin,
                    parameters: schema.parameters,
                }
            })
            .collect();

        // Add external tools that are in the store but not yet in the registry
        for stored in &stored_tools {
            if !stored.builtin && !tools.iter().any(|t| t.name == stored.name) {
                tools.push(ToolInfo {
                    name: stored.name.clone(),
                    description: stored.description.clone(),
                    enabled: stored.enabled,
                    active: true,
                    builtin: false,
                    parameters: stored.parameters.clone(),
                });
            }
        }

        tools.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(tools)
    }

    pub fn show_tool(&self, name: &str) -> Result<ToolInfo, OrbitError> {
        let schema = self
            .tool_registry()
            .get_schema(name)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Tool, name.to_string()))?;

        let stored = self.stores().tools().get_tool(name)?;
        let enabled = stored.is_none_or(|s| s.enabled);
        let active = self.tool_registry().is_active(&schema.name);

        Ok(ToolInfo {
            name: schema.name,
            description: schema.description,
            enabled,
            active,
            builtin: schema.builtin,
            parameters: schema.parameters,
        })
    }

    pub fn add_tool(
        &self,
        name: &str,
        path: &str,
        description: &str,
        parameters: Vec<ToolParam>,
    ) -> Result<(), OrbitError> {
        let p = Path::new(path);
        if !p.exists() {
            return Err(OrbitError::InvalidInput(format!(
                "path does not exist: {path}"
            )));
        }

        if let Some(schema) = self.tool_registry().get_schema(name)
            && schema.builtin
        {
            return Err(OrbitError::InvalidInput(format!(
                "cannot overwrite built-in tool '{name}'"
            )));
        }

        let tool = StoredTool {
            name: name.to_string(),
            path: path.to_string(),
            description: description.to_string(),
            enabled: true,
            builtin: false,
            parameters,
        };

        self.with_mutation(|| {
            self.stores().tools().insert_tool(&tool)?;
            Ok((
                (),
                OrbitEvent::ToolAdded {
                    name: name.to_string(),
                },
            ))
        })
    }

    pub fn remove_tool(&self, name: &str) -> Result<(), OrbitError> {
        if let Some(schema) = self.tool_registry().get_schema(name)
            && schema.builtin
        {
            return Err(OrbitError::InvalidInput(format!(
                "cannot remove built-in tool '{name}'; use 'orbit tool disable {name}' instead"
            )));
        }

        self.with_mutation(|| {
            let deleted = self.stores().tools().delete_tool(name)?;
            if !deleted {
                return Err(OrbitError::not_found(NotFoundKind::Tool, name.to_string()));
            }
            Ok((
                (),
                OrbitEvent::ToolRemoved {
                    name: name.to_string(),
                },
            ))
        })
    }

    pub fn doctor(&self) -> Result<Vec<DoctorResult>, OrbitError> {
        let tools = self.list_tools()?;
        let mut results = Vec::new();

        for tool in &tools {
            if !tool.enabled {
                results.push(DoctorResult {
                    tool_name: tool.name.clone(),
                    status: DoctorStatus::Warning,
                    message: "tool is disabled".to_string(),
                });
                continue;
            }

            if tool.description.is_empty() {
                results.push(DoctorResult {
                    tool_name: tool.name.clone(),
                    status: DoctorStatus::Warning,
                    message: "missing description".to_string(),
                });
                continue;
            }

            if !tool.builtin
                && let Some(stored) = self.stores().tools().get_tool(&tool.name)?
                && !stored.path.is_empty()
            {
                let path = std::path::Path::new(&stored.path);
                if !path.exists() {
                    results.push(DoctorResult {
                        tool_name: tool.name.clone(),
                        status: DoctorStatus::Error,
                        message: format!("executable not found: {}", stored.path),
                    });
                    continue;
                }
            }

            results.push(DoctorResult {
                tool_name: tool.name.clone(),
                status: DoctorStatus::Ok,
                message: String::new(),
            });
        }

        Ok(results)
    }

    pub fn enable_tool(&self, name: &str) -> Result<(), OrbitError> {
        self.set_tool_enabled_state(name, true)
    }

    pub fn disable_tool(&self, name: &str) -> Result<(), OrbitError> {
        self.set_tool_enabled_state(name, false)
    }

    pub fn ensure_tool_agent_facing(&self, name: &str) -> Result<(), OrbitError> {
        if self.tool_registry().is_active(name) {
            return Ok(());
        }
        if self.tool_registry().has(name) {
            return Err(OrbitError::Execution(format!(
                "tool '{name}' is inactive on the agent tool surface; it is an admin/human-only operation not reachable by agents"
            )));
        }
        Err(OrbitError::not_found(NotFoundKind::Tool, name.to_string()))
    }

    fn set_tool_enabled_state(&self, name: &str, enabled: bool) -> Result<(), OrbitError> {
        if !self.tool_registry().has(name) {
            return Err(OrbitError::not_found(NotFoundKind::Tool, name.to_string()));
        }

        let existing = self.stores().tools().get_tool(name)?;
        if !self.tool_registry().is_active(name) {
            let changes_stored_state = existing
                .as_ref()
                .map_or(!enabled, |stored| stored.enabled != enabled);
            if !changes_stored_state {
                let state = if enabled { "enabled" } else { "disabled" };
                let verb = if enabled { "enable" } else { "disable" };
                return Err(OrbitError::InvalidInput(format!(
                    "tool '{name}' is inactive on the agent tool surface and is already {state}; \
                     `orbit tool {verb}` would be a no-op"
                )));
            }
        }

        if existing.is_none() {
            let schema = self
                .tool_registry()
                .get_schema(name)
                .ok_or_else(|| OrbitError::not_found(NotFoundKind::Tool, name.to_string()))?;
            let tool = StoredTool {
                name: name.to_string(),
                path: String::new(),
                description: schema.description.clone(),
                enabled,
                builtin: schema.builtin,
                parameters: schema.parameters.clone(),
            };
            return self.with_mutation(|| {
                self.stores().tools().insert_tool(&tool)?;
                let event = if enabled {
                    OrbitEvent::ToolEnabled {
                        name: name.to_string(),
                    }
                } else {
                    OrbitEvent::ToolDisabled {
                        name: name.to_string(),
                    }
                };
                Ok(((), event))
            });
        }

        self.with_mutation(|| {
            self.stores().tools().set_tool_enabled(name, enabled)?;
            let event = if enabled {
                OrbitEvent::ToolEnabled {
                    name: name.to_string(),
                }
            } else {
                OrbitEvent::ToolDisabled {
                    name: name.to_string(),
                }
            };
            Ok(((), event))
        })
    }
}
