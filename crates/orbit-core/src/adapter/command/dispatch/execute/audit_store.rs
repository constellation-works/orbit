//! The audit-store dispatch core: builds the audit row for one tool call,
//! persists it, and hands the outcome to success finalization.

use std::time::Instant;

use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::contracts::{AuditEventInsertParams, AuditInvocationFields};
use orbit_tools::ToolExecutionKind;
use orbit_tools::plugin::{take_delivered_plugin_secret_names, take_plugin_secret_updates};
use orbit_types::plugin::PluginProvenance;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

use crate::redact_sensitive_env_text;

use super::super::audit::{AuditContext, audit_role_label_for_entry_point, resolve_audit_context};
use super::super::callback::take_callback_plugin_provenance;
use super::entry::{ToolEntryPoint, mark_tool_audit_recorded};
use super::finalize::finalize_successful_dispatch;

/// Outcome of [`crate::OrbitRuntime::execute_tool_command_dispatch`]: the tool's JSON
/// return value plus a flag indicating whether the runtime persisted an audit
/// row for this invocation.
#[derive(Debug)]
pub struct ToolDispatchOutcome {
    pub value: Value,
    pub audit_recorded: bool,
}

pub(super) struct ToolDispatchAuditContext {
    pub(super) agent_override: Option<String>,
    pub(super) model_override: Option<String>,
    pub(super) entry_point: ToolEntryPoint,
    pub(super) session_context: Option<ToolSessionContext>,
    pub(super) brokered: Option<BrokeredAudit>,
}

/// What a call a run's plugin broker executes is recorded under (design
/// `docs/design/plugins/2_agent_call_broker.md` §4.4). Every field comes from
/// the run's dispatch record or the authenticated connection, never from this
/// process's environment or the tool input, which describe the host rather
/// than the agent the call is made for.
pub(in crate::adapter::command::dispatch) struct BrokeredAudit {
    /// The authenticated peer's PID in the host's namespace.
    pub(in crate::adapter::command::dispatch) peer_pid: u32,
    pub(in crate::adapter::command::dispatch) role: String,
    pub(in crate::adapter::command::dispatch) context: AuditContext,
    /// The `cwd` the agent made the call from.
    pub(in crate::adapter::command::dispatch) working_directory: String,
}

pub(super) fn execute_tool_dispatch_with_audit_store<F, S>(
    name: &str,
    input: Value,
    execution_kind: ToolExecutionKind,
    plugin: Option<PluginProvenance>,
    audit: ToolDispatchAuditContext,
    mut persist_audit: S,
    dispatch: F,
) -> Result<ToolDispatchOutcome, OrbitError>
where
    F: FnOnce(Value) -> Result<Value, OrbitError>,
    S: FnMut(&AuditEventInsertParams, AuditInvocationFields<'_>) -> Result<(), OrbitError>,
{
    let ToolDispatchAuditContext {
        agent_override,
        model_override,
        entry_point,
        session_context,
        brokered,
    } = audit;
    let start = Instant::now();
    let (role_label, working_directory, audit_context, brokered_peer_pid) = match brokered {
        Some(brokered) => (
            brokered.role,
            brokered.working_directory,
            brokered.context,
            Some(brokered.peer_pid),
        ),
        None => (
            audit_role_label_for_entry_point(
                &input,
                agent_override.as_deref(),
                model_override.as_deref(),
                entry_point,
            ),
            std::env::current_dir()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".to_string()),
            resolve_audit_context(&input, entry_point, session_context.as_ref()),
            None,
        ),
    };

    // Keep the callback inside the audit boundary so setup, policy, and
    // implementation failures all produce a failure-status row.
    //
    // A plugin call records the names of the secrets its request carried on
    // this thread; anything left from an earlier call is dropped first so
    // this row names only what this call delivered. So does what the call's
    // backend did with `secret_updates`: names and applied/refused only.
    let _ = take_delivered_plugin_secret_names();
    let _ = take_plugin_secret_updates();
    let _ = crate::runtime::application_authority::take_application_audit();
    // Minted before the tool runs so a callback it binds can record this
    // row's id as `entry_execution_id`. The frame pops when this dispatch
    // ends, including when the tool fails.
    let execution_frame = DispatchAuditFrame::enter();
    let result = dispatch(input);
    let application = crate::runtime::application_authority::take_application_audit();
    crate::runtime::application_authority::finish_application_dispatch();
    let plugin_secrets = take_delivered_plugin_secret_names();
    let plugin_secret_updates = take_plugin_secret_updates();
    let plugin = take_callback_plugin_provenance().or(plugin);
    let duration_ms = (start.elapsed().as_millis() as i64).max(1);

    let (status, exit_code, error_message) = match &result {
        Ok(_) => (AuditEventStatus::Success, 0, None),
        // A refusal is not a failure: policy and capability refusals remain
        // queryable as `denied`.
        Err(OrbitError::PolicyDenied(msg) | OrbitError::CapabilityDenied(msg)) => (
            AuditEventStatus::Denied,
            1,
            Some(redact_sensitive_env_text(msg)),
        ),
        Err(error @ OrbitError::DecisionRefused { code, .. }) if code.is_policy_denial() => (
            AuditEventStatus::Denied,
            1,
            Some(redact_sensitive_env_text(&error.to_string())),
        ),
        // A plugin switched off in this workspace is an operator decision,
        // refused before the tool ran, not a failed call.
        Err(err @ OrbitError::PluginDisabledInWorkspace { .. }) => (
            AuditEventStatus::Denied,
            1,
            Some(redact_sensitive_env_text(&err.to_string())),
        ),
        Err(
            err @ OrbitError::NotFound {
                kind: NotFoundKind::Tool,
                ..
            },
        ) if entry_point == ToolEntryPoint::Mcp => (
            AuditEventStatus::Denied,
            1,
            Some(redact_sensitive_env_text(&err.to_string())),
        ),
        Err(err) => (
            AuditEventStatus::Failure,
            1,
            Some(redact_sensitive_env_text(&err.to_string())),
        ),
    };

    let params = AuditEventInsertParams {
        execution_id: execution_frame.id().to_owned(),
        command: "tool".to_string(),
        subcommand: Some(entry_point.audit_subcommand().to_string()),
        tool_name: Some(name.to_string()),
        target_type: Some("tool".to_string()),
        target_id: Some(name.to_string()),
        role: application.as_ref().map_or(role_label, |invocation| {
            format!("application:{}", invocation.consent.application_id)
        }),
        status,
        exit_code,
        duration_ms,
        working_directory,
        arguments_json: None,
        stdout_truncated: None,
        stderr_truncated: None,
        error_message,
        host: std::env::var("HOSTNAME").ok(),
        pid: std::process::id(),
        session_id: audit_context.session_id,
        workspace_id: session_context
            .as_ref()
            .and_then(|context| context.workspace_id.clone()),
        caller_machine_id: session_context
            .as_ref()
            .and_then(|context| context.caller_machine_id.clone()),
        caller_machine_name: session_context
            .as_ref()
            .and_then(|context| context.caller_machine_name.clone()),
        process_machine_id: session_context
            .as_ref()
            .and_then(|context| context.process_machine_id.clone()),
        process_machine_name: session_context
            .as_ref()
            .and_then(|context| context.process_machine_name.clone()),
        transport: session_context
            .as_ref()
            .and_then(|context| context.transport),
        effective_capabilities: session_context
            .as_ref()
            .map(|context| context.effective_capabilities.clone())
            .unwrap_or_default(),
        origin_session_id: session_context
            .as_ref()
            .and_then(|context| context.origin_session_id.clone()),
        mcp_call_id: session_context
            .as_ref()
            .and_then(|context| context.mcp_call_id.clone()),
        // V1 MCP sessions do not carry execution leases.
        lease_id: None,
        task_id: audit_context.task_id,
        job_run_id: audit_context.job_run_id,
        activity_id: audit_context.activity_id,
        step_index: audit_context.step_index,
    };

    // ORB-10890: the session's self-reported actor rides to the audit row and
    // stops there. `role_label` above was computed without it, and no field of
    // `ToolContext` reads it, so an MCP client naming itself cannot move its
    // own trust label off `unverified`.
    // Rebuilt for the lock retry. The fields only borrow this call's
    // session, so a second insert does not observe a different invocation.
    let invocation = || AuditInvocationFields {
        application: application.as_ref(),
        trace_id: session_context
            .as_ref()
            .and_then(|context| context.trace_id.as_deref()),
        caller_ip: session_context
            .as_ref()
            .and_then(|context| context.caller_ip.as_deref()),
        self_reported_actor: session_context
            .as_ref()
            .and_then(|context| context.self_reported_actor.as_deref()),
        plugin: plugin.as_ref(),
        plugin_secrets: &plugin_secrets,
        plugin_secret_updates: Some(&plugin_secret_updates),
        brokered_peer_pid,
    };
    let mut audit_write = persist_audit(&params, invocation());
    // The tool's store and the global audit database are different files, so
    // the audit row cannot join the mutation's transaction. A lock that
    // outlasts one busy timeout is retried once, and only after the tool
    // itself succeeded: a failed call has nothing committed to protect, and
    // its own error must not wait on a second timeout.
    if result.is_ok() && audit_write.as_ref().is_err_and(OrbitError::is_lock_busy) {
        tracing::warn!(
            tool = name,
            "committed tool call lost the audit-store lock; retrying with the busy timeout"
        );
        audit_write = persist_audit(&params, invocation());
    }

    // Claim the row for the runtime the moment it persists, so the CLI
    // `AuditGuard` suppresses its own duplicate emission. This is
    // independent of the tool's own success/failure: the audit row is
    // written for both (a failed call still gets a failure-status row).
    if audit_write.is_ok() {
        mark_tool_audit_recorded();
    }

    // Propagate the tool's own error first. A call that already failed
    // carries no committed mutation to strand, so its error is the
    // authoritative result and the audit-write outcome is irrelevant.
    let value = result?;

    // The tool call succeeded, so any mutation it performed is now
    // committed in its own store. The audit row is a later write to the
    // global audit database. Failing this call does not roll the mutation
    // back, and a caller that retries on the error creates a second object
    // (ORB-16383). Finding M1 (SECURITY-REVIEW-2026-07-15) still holds for
    // the silent case: a missing audit row is reported on the success value.
    finalize_successful_dispatch(name, execution_kind, value, audit_write)
}

/// This dispatch's audit execution id, pushed before the tool runs and popped
/// when the dispatch ends so a nested callback sees it as the entry id.
struct DispatchAuditFrame {
    id: String,
}

impl DispatchAuditFrame {
    fn enter() -> Self {
        Self {
            id: orbit_common::observability::audit_id::push_dispatch_execution_id(),
        }
    }

    fn id(&self) -> &str {
        &self.id
    }
}

impl Drop for DispatchAuditFrame {
    fn drop(&mut self) {
        orbit_common::observability::audit_id::pop_dispatch_execution_id();
    }
}
