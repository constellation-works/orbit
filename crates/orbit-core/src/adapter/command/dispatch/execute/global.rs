//! Server-global and host-global tool dispatch: tools with no workspace
//! runtime behind them, audited through the global audit store.

use std::path::Path;

use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::Store;
use orbit_tools::{ToolContext, ToolExecutionKind};
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

use crate::runtime::tool_exec::{check_activity_tool_policy, check_tool_active};

use super::super::audit::{activity_binding_from_env, managed_run_context};
use super::super::callback::{
    enforce_plugin_callback_allowlist_from_root, read_activity_tool_policy_from_env,
    read_proc_allowed_programs_from_env, read_proc_disallowed_programs_from_env,
};
use super::audit_store::{
    ToolDispatchAuditContext, ToolDispatchOutcome, execute_tool_dispatch_with_audit_store,
};
use super::entry::ToolEntryPoint;
#[cfg(unix)]
use super::entry::{broker_entry_point, mark_tool_audit_recorded};

/// Execute a server-global implementation inside Core's ordinary tool audit
/// boundary without constructing a workspace runtime.
///
/// The callback owns its server-local projection. Core opens only the global
/// audit store, records the supplied invocation context, and preserves the
/// same fail-closed success semantics as runtime-backed tool dispatch.
pub fn execute_global_in_process_tool_dispatch<F>(
    global_root: &Path,
    name: &str,
    input: Value,
    entry_point: ToolEntryPoint,
    session_context: ToolSessionContext,
    dispatch: F,
) -> Result<ToolDispatchOutcome, OrbitError>
where
    F: FnOnce(Value) -> Result<Value, OrbitError>,
{
    execute_tool_dispatch_with_audit_store(
        name,
        input,
        ToolExecutionKind::Mutating,
        None,
        ToolDispatchAuditContext {
            agent_override: None,
            model_override: None,
            entry_point,
            session_context: Some(session_context.clone()),
            brokered: None,
        },
        |params, invocation| {
            let audit_db = orbit_config::resolved_audit_db_path(
                &orbit_config::ConfigRoots::global_only(global_root),
            )?;
            Store::open(&audit_db)?.insert_audit_event_record_with_invocation(params, invocation)
        },
        |input| {
            enforce_plugin_callback_allowlist_from_root(global_root, name)?;
            if session_context.application.is_some()
                || (entry_point == ToolEntryPoint::Cli && std::env::var_os(orbit_common::security::application_credential::APPLICATION_CREDENTIAL_FD_ENV).is_some())
            {
                return Err(OrbitError::CapabilityDenied("application calls require an explicitly resolved workspace runtime".into()));
            }
            dispatch(input)
        },
    )
}

/// Execute one host-global plugin tool inside the audited dispatch boundary.
///
/// The workspace-scoped path goes through `OrbitRuntime`; a `mcp_scope:
/// global` plugin tool has no workspace to open, so its execution context is
/// this process's cwd and the registry the caller composed from the host's
/// installed plugins.
pub(in crate::adapter::command) fn execute_global_plugin_dispatch(
    global_root: &Path,
    name: &str,
    input: Value,
    entry_point: ToolEntryPoint,
    session_context: ToolSessionContext,
    registry: orbit_tools::ToolRegistry,
) -> Result<Value, OrbitError> {
    let binding = registry
        .plugin_binding(name)
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Tool, name.to_string()))?;
    #[cfg(unix)]
    let broker_refusal = if let Some(socket) = std::env::var_os("ORBIT_PLUGIN_BROKER")
        && session_context.application.is_none()
        && std::env::var_os(
            orbit_common::security::application_credential::APPLICATION_CREDENTIAL_FD_ENV,
        )
        .is_none()
    {
        let cwd = std::env::current_dir()?;
        let result = crate::runtime::plugin::broker::forward_call_with_status(
            Path::new(&socket),
            name,
            input.clone(),
            &cwd,
            None,
            broker_entry_point(entry_point),
        );
        match result {
            Ok(value) => {
                mark_tool_audit_recorded();
                return Ok(value);
            }
            Err(crate::runtime::plugin::broker::ForwardCallError::BrokerAudit(error)) => {
                mark_tool_audit_recorded();
                return Err(error);
            }
            Err(crate::runtime::plugin::broker::ForwardCallError::CallerAudit(error)) => {
                Some(error)
            }
        }
    } else {
        None
    };
    let execution_kind = registry
        .execution_kind(name)
        .unwrap_or(ToolExecutionKind::Mutating);
    let global_root = global_root.to_path_buf();
    let trusted_env = entry_point != ToolEntryPoint::Mcp || managed_run_context();
    let activity_tool_policy = if trusted_env {
        read_activity_tool_policy_from_env()
    } else {
        Default::default()
    };
    let tool_context = ToolContext {
        cwd: std::env::current_dir()
            .ok()
            .map(|path| path.to_string_lossy().into_owned()),
        session_context: session_context.clone(),
        allowed_tools: activity_tool_policy.allowed_tools,
        tool_deny_policy: activity_tool_policy.deny_policy,
        proc_allowed_programs: if trusted_env {
            read_proc_allowed_programs_from_env()
        } else {
            Vec::new()
        },
        proc_disallowed_programs: if trusted_env {
            read_proc_disallowed_programs_from_env()
        } else {
            None
        },
        activity_binding: activity_binding_from_env(),
        ..Default::default()
    };
    execute_tool_dispatch_with_audit_store(
        name,
        input,
        execution_kind,
        Some(binding.provenance.clone()),
        ToolDispatchAuditContext {
            agent_override: None,
            model_override: None,
            entry_point,
            session_context: Some(session_context),
            brokered: None,
        },
        |params, invocation| {
            let audit_db = orbit_config::resolved_audit_db_path(
                &orbit_config::ConfigRoots::global_only(&global_root),
            )?;
            Store::open(&audit_db)?.insert_audit_event_record_with_invocation(params, invocation)
        },
        |input| {
            #[cfg(unix)]
            if let Some(error) = broker_refusal {
                return Err(error);
            }
            #[cfg(unix)]
            crate::runtime::plugin::broker::refuse_unbrokered_call(&global_root, name)?;
            // The workspace path authorizes inside `execute_registered_tool`;
            // this one has no runtime, so the same generic plugin row is
            // resolved here — inside the audited closure, so a refusal lands
            // on the row for this call.
            enforce_plugin_callback_allowlist_from_root(&global_root, name)?;
            if orbit_common::security::application_credential::ApplicationCredential::from_process_descriptor()?.is_some() || tool_context.session_context.application.is_some() {
                return Err(OrbitError::CapabilityDenied("application calls require an explicitly resolved workspace runtime".into()));
            }
            authorize_plugin_tool(
                binding.execution_kind == orbit_types::plugin::PluginExecutionKind::Mutating,
                binding.agent_callable,
                entry_point,
                &tool_context.session_context,
            )?;
            check_tool_active(&registry, name)?;
            check_activity_tool_policy(name, &tool_context)?;
            registry.execute(name, &tool_context, input)
        },
    )
    .map(|outcome| outcome.value)
}

/// Decide a plugin tool call that has no workspace runtime behind it.
fn authorize_plugin_tool(
    mutating: bool,
    agent_callable: bool,
    entry_point: ToolEntryPoint,
    session_context: &ToolSessionContext,
) -> Result<(), OrbitError> {
    use orbit_common::governance::authorization::{
        CallerCapabilities, CallerEnvelope, authorize, governed_plugin_tool,
    };

    let envelope = match entry_point {
        // An MCP session's authority is whatever its server was started with,
        // never this process's environment.
        ToolEntryPoint::Mcp => CallerEnvelope::mcp_session(session_context),
        ToolEntryPoint::Cli => CallerEnvelope::from_process_env(session_context),
    };
    let operation = governed_plugin_tool(mutating, agent_callable);
    let caller = CallerCapabilities::resolve_for_operation(&envelope, operation);
    authorize(operation, &caller).map_err(|denial| OrbitError::CapabilityDenied(denial.to_string()))
}
