//! Shared entry adapter for CLI, Web, tool-host, and engine-host tool calls.

use crate::app::resident;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::redaction::{redact_all_error, redact_sensitive_env_json};
use orbit_tools::ToolContext;
use orbit_types::record::OrbitEvent;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::contracts::ToolInvocationPort;
use crate::runtime::tool_exec::{
    CapabilityEnforcement, RegisteredToolAdmission, check_activity_tool_policy,
    corrupted_drain_identity, populate_filesystem_policy_context, resolve_task_id_from_context,
};
use crate::runtime::{check_worker_host_input, is_coordination_tool};

impl OrbitRuntime {
    /// In-process owner transport. The accepting runtime still applies the
    /// worker session's transaction fence; only execution-host routing is removed.
    pub fn execute_owner_coordination(
        &self,
        name: &str,
        input: Value,
        mut session: orbit_types::tool::ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let binding = session
            .worker_invocation
            .as_ref()
            .ok_or_else(|| OrbitError::PolicyDenied("worker context required".into()))?;
        if self.automation_machine_identity() != Some(binding.owner_machine_id.as_str()) {
            return Err(OrbitError::PolicyDenied(
                "owner process identity mismatch".into(),
            ));
        }
        let owner = self.without_worker_routing();
        session.process_machine_id = self.automation_machine_identity().map(str::to_owned);
        session
            .effective_capabilities
            .remove(&orbit_types::tool::McpCapability::Operator);
        // The worker routes the operation it resolved; the owner's dispatch
        // takes the family call a caller makes.
        let (name, input) = orbit_types::tool::tool_family_call(name, input);
        owner
            .execute_tool_command_dispatch_with_session_context(
                &name,
                input,
                None,
                None,
                crate::adapter::command::ToolEntryPoint::Mcp,
                session,
            )
            .map(|outcome| outcome.value)
    }

    pub(crate) fn execute_worker_projection(
        &self,
        name: &str,
        input: &Value,
        session: &orbit_types::tool::ToolSessionContext,
    ) -> Result<Option<Value>, OrbitError> {
        super::tool_host::worker_tools::execute(
            self,
            session,
            orbit_tools::OrbitBuiltinAction::Task(match name {
                "orbit.task.update" => orbit_common::governance::task::TaskVerb::Update,
                "orbit.task.artifact.get" => orbit_common::governance::task::TaskVerb::ArtifactGet,
                _ => orbit_common::governance::task::TaskVerb::Show,
            }),
            input,
        )
    }

    pub fn run_tool(&self, name: &str, input: Value) -> Result<Value, OrbitError> {
        self.run_tool_with_context(name, input, ToolContext::default())
    }

    /// Run a registered tool for a human-operated in-process adapter.
    /// The trusted label lives in `ToolContext`, never in agent-controlled
    /// input, so the ordinary public tool path remains family-validated.
    pub fn run_tool_as_human(&self, name: &str, input: Value) -> Result<Value, OrbitError> {
        self.run_tool_with_context(
            name,
            input,
            ToolContext {
                trusted_actor_label: Some("human".to_string()),
                ..Default::default()
            },
        )
    }

    /// Run a registered tool for an in-process adapter that speaks for its
    /// own caller, such as the dashboard server. `session` is the whole
    /// authority envelope: the tool chokepoint and the use case read its
    /// capabilities and bindings, never the hosting process's terminal or
    /// environment, so a server launched from a shell does not become that
    /// shell's operator.
    pub(crate) fn run_tool_with_session(
        &self,
        name: &str,
        input: Value,
        session: orbit_types::tool::ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.run_tool_with_context(
            name,
            input,
            ToolContext {
                session_context: session,
                ..Default::default()
            },
        )
    }

    pub(crate) fn run_tool_with_context(
        &self,
        name: &str,
        input: Value,
        tool_context: ToolContext,
    ) -> Result<Value, OrbitError> {
        self.run_tool_with_context_and_capability(
            name,
            input,
            tool_context,
            CapabilityEnforcement::Enforce,
        )
    }

    pub(crate) fn run_tool_with_context_and_capability(
        &self,
        name: &str,
        input: Value,
        mut tool_context: ToolContext,
        capability_enforcement: CapabilityEnforcement,
    ) -> Result<Value, OrbitError> {
        self.bind_worker_session(&mut tool_context.session_context)?;
        let application = self.admit_application_call(
            name,
            &input,
            &mut tool_context.session_context,
            capability_enforcement,
        )?;
        if application {
            let session = &tool_context.session_context;
            if let Some(output) = self.execute_application_builtin(name, &input, session)? {
                return Ok(output);
            }
            if let Some(output) = self.execute_application_read(name, &input, session)? {
                return Ok(output);
            }
        }
        if !self.tool_registry().has(name) {
            self.ensure_tool_agent_facing_with_context(name, &tool_context)?;
        }
        if tool_context.orbit_host.is_none() {
            let task_id = resolve_task_id_from_context(self, &tool_context)?;
            tool_context.orbit_host = Some(super::tool_host::build_orbit_tool_host(
                crate::Core::within_tool_entry(self.clone()),
                task_id,
                super::tool_host::ToolHostRun::InProcess(tool_context.activity_binding.clone()),
                tool_context.session_context.clone(),
            ));
        }
        self.execute_registered_tool(name, input, tool_context, capability_enforcement)
    }

    /// Admission shared by dispatch and dry-run, including input-dependent
    /// capability floors. This never invokes a tool implementation.
    pub(crate) fn authorize_registered_tool(
        &self,
        name: &str,
        input: &Value,
        tool_context: &ToolContext,
        capability_enforcement: CapabilityEnforcement,
    ) -> Result<(), OrbitError> {
        let resident_drain_status =
            match self.admit_registered_tool(name, input, tool_context, capability_enforcement)? {
                RegisteredToolAdmission::Admitted => return Ok(()),
                RegisteredToolAdmission::Capability {
                    resident_drain_status_run: Some(run_id),
                } => resident::resident_cycle_drain_status_allowed(self, &run_id, input)?,
                RegisteredToolAdmission::Capability {
                    resident_drain_status_run: None,
                } => false,
            };
        self.authorize_admitted_tool(
            name,
            input,
            tool_context,
            capability_enforcement,
            resident_drain_status,
        )
    }

    /// Registered tool dispatch: routes a claimed worker's coordination call
    /// through the owner coordinator and dispatches local registry
    /// executions.
    fn execute_registered_tool(
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
        if is_coordination_tool(name) {
            check_worker_host_input(&input, tool_context.session_context.worker_host_call)?;
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

        if let Some(owner) = self.run().claim_owner()
            && is_coordination_tool(name)
        {
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
            return owner.call(name, input, tool_context.session_context);
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

        if self.tool_registry().plugin_binding(name).is_some() {
            // Callback sessions inherit the workspace this host resolved.
            tool_context.session_context.workspace_id = self.workspace_id().ok();
            if tool_context.trusted_actor_label.is_none() {
                tool_context.trusted_actor_label = self
                    .actor()
                    .resolve_write_label(
                        tool_context.agent_name.as_deref(),
                        tool_context.model_name.as_deref(),
                    )
                    .ok();
            }
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

/// Application use cases reach tool execution through this port [ORB-22004].
/// Each method delegates to the inherent entry point of the same behaviour, so
/// audit rows and capability checks are unchanged.
impl ToolInvocationPort for OrbitRuntime {
    fn run_tool_command_with_session(
        &self,
        name: &str,
        input: Value,
        agent_override: Option<String>,
        model_override: Option<String>,
        session_context: orbit_types::tool::ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.execute_tool_command_with_session_context(
            name,
            input,
            agent_override,
            model_override,
            session_context,
        )
    }

    fn run_tool_with_context(
        &self,
        name: &str,
        input: Value,
        tool_context: ToolContext,
    ) -> Result<Value, OrbitError> {
        self.run_tool_with_context(name, input, tool_context)
    }

    fn run_tool_with_enforcement(
        &self,
        name: &str,
        input: Value,
        tool_context: ToolContext,
        enforcement: CapabilityEnforcement,
    ) -> Result<Value, OrbitError> {
        self.run_tool_with_context_and_capability(name, input, tool_context, enforcement)
    }

    fn authorize_tool(
        &self,
        name: &str,
        input: &Value,
        tool_context: &ToolContext,
        enforcement: CapabilityEnforcement,
    ) -> Result<(), OrbitError> {
        self.authorize_registered_tool(name, input, tool_context, enforcement)
    }
}
