//! Grant-time tool admission shared by task creation and grant replacement.

use orbit_common::OrbitError;
use orbit_common::governance::authorization::{self, ToolGrantability};
use orbit_types::task::{GRANTED_TOOLS_FIELD, normalize_required_tools};
use orbit_types::tool::McpCapability;

use super::TaskJobPort;
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Validate an operator's grant before persisting it. Unlike durable
    /// requirements, grants refuse disabled tools rather than warning: every
    /// named tool must actually be available to the implementing agent.
    pub(super) fn validate_granted_tools(&self, names: &[String]) -> Result<(), OrbitError> {
        for name in normalize_required_tools(names.to_vec()) {
            if authorization::classify_agent_tool_grant(&name).grantability
                == ToolGrantability::AlwaysOperatorOnly
            {
                let reason = authorization::governed_tool(&name)
                    .filter(|row| !row.allowed.contains(&McpCapability::Agent))
                    .map(|row| {
                        format!(
                            "governed row '{}' does not admit the agent capability",
                            row.id
                        )
                    })
                    .unwrap_or_else(|| "the tool is classified as always operator-only".into());
                return Err(OrbitError::InvalidInput(format!(
                    "{GRANTED_TOOLS_FIELD} cannot grant '{name}': {reason}"
                )));
            }
            if let Some(reason) = self.task_tool_availability_refusal(&name)? {
                return Err(OrbitError::InvalidInput(format!(
                    "{GRANTED_TOOLS_FIELD} cannot grant '{name}': {reason}"
                )));
            }
        }
        Ok(())
    }
}
