//! Preview CLI tool admission without invoking its implementation.

use orbit_common::security::redaction::redact_all_error;
use orbit_tools::ToolContext;
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

use crate::runtime::tool_exec::{
    CapabilityEnforcement, check_activity_tool_policy, populate_filesystem_policy_context,
};
use crate::{NotFoundKind, OrbitError, OrbitRuntime};

use super::callback::{
    enforce_plugin_callback_allowlist, read_activity_tool_policy_from_env,
    take_callback_plugin_provenance,
};

impl OrbitRuntime {
    /// Preview admission using the ordinary CLI caller's process envelope.
    pub fn run_tool_dry_run(&self, name: &str, input: &Value) -> Result<DryRunResult, OrbitError> {
        self.run_tool_dry_run_with_session_context(name, input, ToolSessionContext::default())
    }

    /// Preview the same capability, activity, active-tool and plugin-callback
    /// checks as CLI dispatch. Tool bodies and their input-specific validation
    /// are never invoked; capability decisions retain their ordinary audit.
    pub fn run_tool_dry_run_with_session_context(
        &self,
        name: &str,
        input: &Value,
        session_context: ToolSessionContext,
    ) -> Result<DryRunResult, OrbitError> {
        let schema = self
            .tool_registry()
            .get_schema(name)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Tool, name.to_string()))?;

        let admission = (|| {
            let callback = enforce_plugin_callback_allowlist(
                &self.global_root(),
                self.stores().plugins(),
                name,
            );
            // Dispatch consumes this attribution when it writes its audit row.
            // A preview must not leave it on the thread for a later call.
            take_callback_plugin_provenance();
            callback?;
            self.ensure_tool_agent_facing(name)?;

            let activity_policy = read_activity_tool_policy_from_env();
            let mut context = ToolContext {
                cwd: std::env::current_dir()
                    .ok()
                    .map(|cwd| cwd.to_string_lossy().into_owned()),
                session_context,
                allowed_tools: activity_policy.allowed_tools,
                tool_deny_policy: activity_policy.deny_policy,
                ..Default::default()
            };
            self.bind_worker_session(&mut context.session_context)?;
            populate_filesystem_policy_context(self, &mut context)?;
            self.authorize_registered_tool(name, input, &context, CapabilityEnforcement::Enforce)?;
            check_activity_tool_policy(name, &context)
        })();
        let policy_denial_reason = match admission.map_err(redact_all_error) {
            Ok(()) => None,
            Err(OrbitError::PolicyDenied(reason) | OrbitError::CapabilityDenied(reason)) => {
                Some(reason)
            }
            Err(error) => return Err(error),
        };
        let missing_params = schema
            .parameters
            .iter()
            .filter(|param| param.required && input.get(&param.name).is_none())
            .map(|param| param.name.clone())
            .collect();

        Ok(DryRunResult {
            tool_name: name.to_string(),
            policy_allowed: policy_denial_reason.is_none(),
            policy_denial_reason,
            missing_params,
        })
    }
}

/// Tool admission and required-input presence, without execution.
#[derive(Debug, Clone)]
pub struct DryRunResult {
    pub tool_name: String,
    pub policy_allowed: bool,
    pub policy_denial_reason: Option<String>,
    pub missing_params: Vec<String>,
}
