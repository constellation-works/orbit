//! Run-layer registered tool dispatch on [`RuntimeKernel`]: routes claimed
//! workers' coordination calls through the owner coordinator and dispatches
//! local registry executions.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::redaction::{redact_all_error, redact_sensitive_env_json};
use orbit_tools::ToolContext;
use orbit_types::record::OrbitEvent;
use serde_json::Value;

use super::worker_coordination;
use crate::RuntimeKernel;
use crate::host::exec::tool_exec::{
    CapabilityEnforcement, check_activity_tool_policy, corrupted_drain_identity,
    populate_filesystem_policy_context,
};

impl RuntimeKernel {
    pub fn execute_registered_tool(
        &self,
        name: &str,
        input: Value,
        mut tool_context: ToolContext,
        capability_enforcement: CapabilityEnforcement,
    ) -> Result<Value, OrbitError> {
        // Every check below keys on the operation a family call reaches.
        let (operation, input) = if orbit_types::tool::tool_family(name).is_some() {
            self.tool_registry().resolve_tool_call(name, input)?
        } else {
            (name.to_string(), input)
        };
        let name = operation.as_str();
        if worker_coordination::is_coordination_tool(name) {
            worker_coordination::check_worker_host_input(
                &input,
                tool_context.session_context.worker_host_call,
            )?;
        }
        if tool_context.cwd.is_none() {
            tool_context.cwd = std::env::current_dir()
                .ok()
                .map(|cwd| cwd.to_string_lossy().into_owned());
        }

        populate_filesystem_policy_context(self, &mut tool_context)?;

        self.authorize_registered_tool(name, &input, &tool_context, capability_enforcement)?;
        check_activity_tool_policy(name, &tool_context).map_err(|error| match error {
            OrbitError::PolicyDenied(reason) => {
                self.record_activity_tool_policy_denial(
                    name,
                    &reason,
                    &tool_context.session_context,
                    capability_enforcement,
                );
                self.deny_activity_tool(name, reason)
            }
            error => error,
        })?;

        if self.worker_invocation().is_some() && worker_coordination::is_coordination_tool(name) {
            let input = if name == "orbit.task.artifact.put" {
                orbit_tools::prepare_remote_task_artifact_put(
                    input,
                    tool_context.cwd.as_deref().map(Path::new),
                    tool_context.workspace_root.as_deref(),
                    tool_context.session_context.transport,
                )?
            } else {
                input
            };
            return self.route_worker_tool(name, input, tool_context.session_context);
        }

        if ((matches!(name, "orbit.task.show" | "orbit.task.artifact.get")
            && input.get("_worker_read").is_some())
            || (name == "orbit.task.update"
                && (input.get("_worker_update").is_some()
                    || input.get("_worker_steering").is_some())))
            && let Some(output) =
                self.execute_worker_projection(name, &input, &tool_context.session_context)?
        {
            return Ok(output);
        }

        let output = match self
            .tool_registry()
            .execute(name, &tool_context, input)
            .map_err(redact_all_error)
        {
            Ok(output) => output,
            Err(error) if error.is_policy_denial() => {
                self.with_mutation(|| {
                    Ok((
                        (),
                        OrbitEvent::PolicyDenied {
                            tool: name.to_string(),
                        },
                    ))
                })?;
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        let output = redact_sensitive_env_json(output);

        self.with_mutation(|| {
            Ok((
                (),
                OrbitEvent::ToolExecuted {
                    name: name.to_string(),
                },
            ))
        })?;

        if (name == "orbit.task.pull" || name.starts_with("orbit.drain."))
            && let Some(field) = corrupted_drain_identity(&output)
        {
            // A mutating call may already have committed. Its caller must
            // reconcile/replay the same request, never invent a replacement.
            let message = format!(
                "owner reply identity field `{field}` contains an environment redaction artefact; retry the same request"
            );
            return Err(match name {
                "orbit.drain.probe" | "orbit.drain.receipt.lookup" | "orbit.drain.claims" => {
                    OrbitError::OwnerNegotiation(message)
                }
                _ => OrbitError::OutcomeUnknown {
                    mcp_call_id: tool_context
                        .session_context
                        .mcp_call_id
                        .clone()
                        .unwrap_or_else(|| format!("redacted-reply:{name}")),
                    message,
                },
            });
        }

        Ok(output)
    }

    /// Record an activity tool-policy refusal and build its error. An audit
    /// write failure surfaces instead, so a refusal is never unrecorded.
    fn deny_activity_tool(&self, name: &str, reason: String) -> OrbitError {
        match self.with_mutation(|| {
            Ok((
                (),
                OrbitEvent::PolicyDenied {
                    tool: name.to_string(),
                },
            ))
        }) {
            Ok(()) => OrbitError::PolicyDenied(reason),
            Err(error) => error,
        }
    }
}
