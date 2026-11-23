//! Grant-time tool admission shared by task creation and grant replacement.

use orbit_common::OrbitError;
use orbit_common::governance::authorization::{self, ToolGrantability};
use orbit_types::task::{GRANTED_TOOLS_FIELD, normalize_required_tools};
use orbit_types::tool::{McpCapability, tool_policy_operations};

use super::TaskJobPort;
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Validate an operator's grant before persisting it. Unlike durable
    /// requirements, grants refuse disabled tools rather than warning: every
    /// named tool must actually be available to the implementing agent.
    pub(crate) fn validate_granted_tools(&self, names: &[String]) -> Result<(), OrbitError> {
        for name in normalize_required_tools(names.to_vec()) {
            if authorization::classify_agent_tool_grant(&name).grantability
                == ToolGrantability::AlwaysOperatorOnly
            {
                let reason = tool_policy_operations(&name)
                    .into_iter()
                    .filter_map(authorization::governed_tool)
                    .find(|row| !row.allowed.contains(&McpCapability::Agent))
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

    /// Refuse a run-bound tool in `field`. Such a tool is served only to
    /// the managed runs Orbit itself mints for it, so no operator or task
    /// may name it as a requirement or a grant.
    pub(crate) fn refuse_run_bound_tool_names(
        &self,
        field: &str,
        names: &[String],
    ) -> Result<(), OrbitError> {
        match normalize_required_tools(names.to_vec())
            .into_iter()
            .find(|name| self.tool_registry().is_run_bound(name))
        {
            Some(name) => Err(OrbitError::InvalidInput(format!(
                "{field} cannot name '{name}': it is served only to the runs Orbit starts for it"
            ))),
            None => Ok(()),
        }
    }
}
