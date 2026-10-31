//! `OrbitRuntime` tool entry methods: the CLI and in-process entry points and
//! the runtime-backed dispatch that resolves identity, policy and capability.

#[cfg(unix)]
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_tools::{ToolContext, ToolExecutionKind};
use orbit_types::policy::Role;
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::tool_exec::{CapabilityEnforcement, populate_filesystem_policy_context};

use super::super::audit::{
    activity_binding_from_env, managed_run_context, reservation_owner_from_env,
    resolve_agent_identity_for_entry_point,
};
use super::super::callback::{
    enforce_plugin_callback_allowlist, read_activity_tool_policy_from_env,
    read_proc_allowed_programs_from_env, read_proc_disallowed_programs_from_env,
    read_proc_spawn_budget_from_env,
};
use super::audit_store::{
    BrokeredAudit, ToolDispatchAuditContext, ToolDispatchOutcome,
    execute_tool_dispatch_with_audit_store,
};
#[cfg(unix)]
use super::entry::broker_entry_point;
use super::entry::{ToolEntryPoint, mark_tool_audit_recorded, tool_suggestion_context};

impl OrbitRuntime {
    /// Execute a tool by name and return its JSON value. CLI-callers use this
    /// path; the runtime tags the audit row with [`ToolEntryPoint::Cli`].
    pub fn execute_tool_command(
        &self,
        name: &str,
        input: Value,
        agent_override: Option<String>,
        model_override: Option<String>,
    ) -> Result<Value, OrbitError> {
        self.execute_tool_command_dispatch(
            name,
            input,
            agent_override,
            model_override,
            ToolEntryPoint::Cli,
        )
        .map(|outcome| outcome.value)
    }

    /// Execute a local CLI tool call with a caller-supplied invocation
    /// envelope. The CLI owns machine identity discovery; Core owns dispatch
    /// and persists the resulting audit context.
    pub fn execute_tool_command_with_session_context(
        &self,
        name: &str,
        input: Value,
        agent_override: Option<String>,
        model_override: Option<String>,
        session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.execute_tool_command_dispatch_with_session_context(
            name,
            input,
            agent_override,
            model_override,
            ToolEntryPoint::Cli,
            session_context,
        )
        .map(|outcome| outcome.value)
    }

    /// Execute a tool by name and return both the value and whether the
    /// runtime persisted an audit row. Callers that need to suppress a
    /// duplicate higher-level audit emission read `audit_recorded` (also
    /// available out-of-band via [`super::take_tool_audit_recorded`]).
    pub fn execute_tool_command_dispatch(
        &self,
        name: &str,
        input: Value,
        agent_override: Option<String>,
        model_override: Option<String>,
        entry_point: ToolEntryPoint,
    ) -> Result<ToolDispatchOutcome, OrbitError> {
        self.execute_tool_command_dispatch_with_session_context(
            name,
            input,
            agent_override,
            model_override,
            entry_point,
            ToolSessionContext::default(),
        )
    }

    pub fn execute_tool_command_dispatch_with_session_context(
        &self,
        name: &str,
        input: Value,
        agent_override: Option<String>,
        model_override: Option<String>,
        entry_point: ToolEntryPoint,
        mut session_context: ToolSessionContext,
    ) -> Result<ToolDispatchOutcome, OrbitError> {
        // The authorization decision and entry-point row describe one call.
        // CLI invocations have no transport-minted trace, so create it here
        // before either path receives the session (and preserve MCP's trace).
        if session_context
            .trace_id
            .as_deref()
            .is_none_or(str::is_empty)
        {
            session_context.trace_id = Some(audit_execution_id("tool-call"));
        }
        // A plugin backend's decision-channel callbacks read a store its
        // sandbox denies, so its descendants hand them to the host that
        // spawned it, which audits what it dispatches. A route that did not
        // answer is this caller's to audit; never answered locally instead.
        if let Some(outcome) =
            orbit_tools::plugin::forward_host_routed_callback(&self.global_root(), name, &input)
        {
            let error = match outcome {
                orbit_tools::plugin::HostRouteOutcome::Answered(result) => {
                    mark_tool_audit_recorded();
                    return result.map(|value| ToolDispatchOutcome {
                        value,
                        audit_recorded: false,
                    });
                }
                orbit_tools::plugin::HostRouteOutcome::Unanswered(error) => error,
            };
            return self.execute_tool_dispatch_with(
                name,
                input,
                ToolDispatchAuditContext {
                    agent_override,
                    model_override,
                    entry_point,
                    session_context: Some(session_context),
                    brokered: None,
                },
                |_| Err(error),
            );
        }
        // The broker audits dispatched plugin calls and host-credentialed
        // reads. Listener refusals and unusable responses belong to this
        // caller's audit boundary; never execute them locally as a fallback.
        #[cfg(unix)]
        let host_read = crate::runtime::plugin::broker::is_host_credentialed_read(name);
        // [ORB-20566] A managed agent's explorer is submitted by its run's
        // broker: started from this sandboxed process, the explorer's worker
        // would create its own sandbox nested in this one, which the kernel
        // refuses. Only the submission is brokered; the wait for its answer
        // stays here, so it holds no broker connection.
        #[cfg(unix)]
        let explorer = name == crate::runtime::plugin::broker::BROKERED_EXPLORER_TOOL
            && crate::runtime::run_input::managed_run_context_run_id_from_env().is_some();
        #[cfg(unix)]
        if let Some(socket) = std::env::var_os("ORBIT_PLUGIN_BROKER")
            && session_context.application.is_none()
            && std::env::var_os(
                orbit_common::security::application_credential::APPLICATION_CREDENTIAL_FD_ENV,
            )
            .is_none()
            && (host_read || explorer || self.tool_registry().plugin_binding(name).is_some())
        {
            let explorer_wait = if explorer {
                match crate::adapter::tool_host::agent_tools::brokered_explorer_wait_seconds(&input)
                {
                    Ok(wait) => wait,
                    Err(error) => {
                        return self.execute_tool_dispatch_with(
                            name,
                            input,
                            ToolDispatchAuditContext {
                                agent_override,
                                model_override,
                                entry_point,
                                session_context: Some(session_context),
                                brokered: None,
                            },
                            |_| Err(error),
                        );
                    }
                }
            } else {
                None
            };
            let mut forwarded = input.clone();
            if explorer && let Some(fields) = forwarded.as_object_mut() {
                fields.remove("wait_seconds");
            }
            let cwd = std::env::current_dir()?;
            // The CLI/MCP adapter already resolved an explicit workspace
            // selector to this runtime. Send its logical identity so the
            // broker can reject a selector for another run, even when the
            // original spelling was a checkout path.
            let workspace = input
                .get("workspace")
                .and_then(Value::as_str)
                .map(|_| self.workspace_id())
                .transpose()?;
            let result = crate::runtime::plugin::broker::forward_call_with_status(
                Path::new(&socket),
                name,
                forwarded,
                &cwd,
                workspace.as_deref(),
                broker_entry_point(entry_point),
            );
            let value = match result {
                Ok(value) => {
                    mark_tool_audit_recorded();
                    value
                }
                Err(crate::runtime::plugin::broker::ForwardCallError::BrokerAudit(error)) => {
                    mark_tool_audit_recorded();
                    return Err(error);
                }
                Err(crate::runtime::plugin::broker::ForwardCallError::CallerAudit(error)) => {
                    return self.execute_tool_dispatch_with(
                        name,
                        input,
                        ToolDispatchAuditContext {
                            agent_override,
                            model_override,
                            entry_point,
                            session_context: Some(session_context),
                            brokered: None,
                        },
                        |_| Err(error),
                    );
                }
            };
            let value = match explorer_wait {
                Some(seconds) => crate::adapter::tool_host::agent_tools::wait_for_explorer_answer(
                    self, value, seconds,
                )?,
                None => value,
            };
            return Ok(ToolDispatchOutcome {
                value,
                audit_recorded: false,
            });
        }
        // A claimed worker's owner calls go to the broker too: the claim's
        // owner is reached over SSH, which the sandbox masks. The broker
        // writes the audit row for what it answers, naming this caller as the
        // peer; a refusal made here is audited below like any other.
        #[cfg(unix)]
        let mut claimed_owner_refusal = None;
        #[cfg(unix)]
        match if session_context.application.is_some()
            || std::env::var_os(
                orbit_common::security::application_credential::APPLICATION_CREDENTIAL_FD_ENV,
            )
            .is_some()
        {
            None
        } else {
            super::super::claimed::bridge_claimed_owner_call(
                &self.global_root(),
                self.worker_invocation(),
                self.automation_machine_identity(),
                name,
                &input,
                &std::env::current_dir()?,
                &self.paths().repo_root,
                entry_point,
            )
        } {
            Some(super::super::claimed::ClaimedOwnerRoute::Forwarded(result)) => {
                mark_tool_audit_recorded();
                return Ok(ToolDispatchOutcome {
                    value: result?,
                    audit_recorded: false,
                });
            }
            Some(super::super::claimed::ClaimedOwnerRoute::Refused(error)) => {
                claimed_owner_refusal = Some(error);
            }
            None => {}
        }
        let audit_session_context = session_context.clone();
        self.execute_tool_dispatch_with(
            name,
            input,
            ToolDispatchAuditContext {
                agent_override: agent_override.clone(),
                model_override: model_override.clone(),
                entry_point,
                session_context: Some(audit_session_context),
                brokered: None,
            },
            |input| {
                let enforcement = match entry_point {
                    ToolEntryPoint::Cli => CapabilityEnforcement::Enforce,
                    ToolEntryPoint::Mcp => CapabilityEnforcement::McpSessionOnly,
                };
                let application =
                    self.admit_application_call(name, &input, &mut session_context, enforcement)?;
                if application
                    && let Some(output) =
                        self.execute_application_builtin(name, &input, &session_context)?
                {
                    return Ok(output);
                }
                #[cfg(unix)]
                if self.tool_registry().plugin_binding(name).is_some() {
                    crate::runtime::plugin::broker::refuse_unbrokered_call(
                        &self.global_root(),
                        name,
                    )?;
                }
                self.ensure_tool_agent_facing_with_context(
                    name,
                    &tool_suggestion_context(entry_point, session_context.clone()),
                )?;
                #[cfg(unix)]
                if let Some(error) = claimed_owner_refusal {
                    return Err(error);
                }
                #[cfg(unix)]
                if host_read {
                    crate::runtime::plugin::broker::refuse_unbrokered_host_read(
                        &self.global_root(),
                        name,
                    )?;
                }
                let trusted_env = entry_point != ToolEntryPoint::Mcp || managed_run_context();
                let activity_tool_policy = if trusted_env {
                    read_activity_tool_policy_from_env()
                } else {
                    Default::default()
                };
                let (agent_name, model_name) = resolve_agent_identity_for_entry_point(
                    entry_point,
                    agent_override,
                    model_override,
                )?;
                let proc_allowed_programs = if trusted_env {
                    read_proc_allowed_programs_from_env()
                } else {
                    Vec::new()
                };
                let proc_disallowed_programs = if trusted_env {
                    read_proc_disallowed_programs_from_env()
                } else {
                    None
                };
                let proc_spawn_activity_scoped = managed_run_context();
                // Only a managed run's envelope carries an activity deadline;
                // an interactive call keeps the unscoped ceiling.
                let proc_spawn_budget = if proc_spawn_activity_scoped {
                    read_proc_spawn_budget_from_env(self.proc_spawn_max_timeout_ms())
                } else {
                    None
                };
                let proc_spawn_environment = Some(self.agent_environment(&[]));
                let cwd = std::env::current_dir()
                    .ok()
                    .map(|path| path.to_string_lossy().into_owned());
                let mut tool_context = ToolContext {
                    cwd,
                    session_context,
                    allowed_tools: activity_tool_policy.allowed_tools,
                    tool_deny_policy: activity_tool_policy.deny_policy,
                    agent_name,
                    model_name,
                    workspace_root: None,
                    proc_allowed_programs,
                    proc_disallowed_programs,
                    proc_spawn_environment,
                    proc_spawn_activity_scoped,
                    proc_spawn_budget,
                    reservation_owner: reservation_owner_from_env(),
                    activity_binding: activity_binding_from_env(),
                    ..Default::default()
                };
                if proc_spawn_activity_scoped {
                    populate_filesystem_policy_context(self, &mut tool_context)?;
                }
                let capability_enforcement = match entry_point {
                    ToolEntryPoint::Cli => CapabilityEnforcement::Enforce,
                    ToolEntryPoint::Mcp => CapabilityEnforcement::McpSessionOnly,
                };
                self.run_tool_with_context_and_role_and_capability(
                    name,
                    input,
                    Role::Admin,
                    tool_context,
                    capability_enforcement,
                )
            },
        )
    }

    /// Run an in-process tool implementation inside the same audit boundary
    /// used by registry-backed tool dispatch.
    ///
    /// Transport adapters use this for tools whose implementation deliberately
    /// lives outside the runtime registry. Policy checks belong inside
    /// `dispatch` so a rejection is captured by the resulting audit row.
    pub fn execute_in_process_tool_dispatch<F>(
        &self,
        name: &str,
        input: Value,
        entry_point: ToolEntryPoint,
        session_context: ToolSessionContext,
        dispatch: F,
    ) -> Result<ToolDispatchOutcome, OrbitError>
    where
        F: FnOnce(Value) -> Result<Value, OrbitError>,
    {
        self.execute_tool_dispatch_with(
            name,
            input,
            ToolDispatchAuditContext {
                agent_override: None,
                model_override: None,
                entry_point,
                session_context: Some(session_context),
                brokered: None,
            },
            dispatch,
        )
    }

    /// Run a plugin call a run's broker admitted inside the audit boundary,
    /// recording it under `brokered` instead of this process's context.
    /// Policy checks belong inside `dispatch` so a refusal lands on the row.
    pub(in crate::adapter::command::dispatch) fn execute_brokered_dispatch<F>(
        &self,
        name: &str,
        input: Value,
        entry_point: ToolEntryPoint,
        session_context: ToolSessionContext,
        brokered: BrokeredAudit,
        dispatch: F,
    ) -> Result<ToolDispatchOutcome, OrbitError>
    where
        F: FnOnce(Value) -> Result<Value, OrbitError>,
    {
        self.execute_tool_dispatch_with(
            name,
            input,
            ToolDispatchAuditContext {
                agent_override: None,
                model_override: None,
                entry_point,
                session_context: Some(session_context),
                brokered: Some(brokered),
            },
            dispatch,
        )
    }

    fn execute_tool_dispatch_with<F>(
        &self,
        name: &str,
        input: Value,
        audit: ToolDispatchAuditContext,
        dispatch: F,
    ) -> Result<ToolDispatchOutcome, OrbitError>
    where
        F: FnOnce(Value) -> Result<Value, OrbitError>,
    {
        let execution_kind = self
            .tool_registry()
            .execution_kind(name)
            .unwrap_or(ToolExecutionKind::Mutating);
        // A plugin-backed entry stamps its identity on the audit row (design
        // `docs/design/plugins/1_scope.md` §4.4). The registry is the only
        // source: tool input never names a plugin.
        let plugin = self
            .tool_registry()
            .plugin_binding(name)
            .map(|binding| binding.provenance.clone());
        let mut application_session = audit.session_context.clone().unwrap_or_default();
        let application_enforcement = match audit.entry_point {
            ToolEntryPoint::Cli => CapabilityEnforcement::Enforce,
            ToolEntryPoint::Mcp => CapabilityEnforcement::McpSessionOnly,
        };
        execute_tool_dispatch_with_audit_store(
            name,
            input,
            execution_kind,
            plugin,
            audit,
            |params, invocation| self.record_audit_event_with_invocation(params, invocation),
            |input| {
                self.admit_application_call(
                    name,
                    &input,
                    &mut application_session,
                    application_enforcement,
                )?;
                enforce_plugin_callback_allowlist(
                    &self.global_root(),
                    self.stores().plugins(),
                    name,
                )?;
                dispatch(input)
            },
        )
    }
}
