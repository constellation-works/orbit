//! Scoped task-write provenance and owner-side `task.mutation` audit rows.

use std::cell::RefCell;
use std::ffi::OsStr;

use orbit_common::OrbitError;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_store::AuditEventInsertParams;
use orbit_store::contract::AuditInvocationFields;
use orbit_types::task::{TaskWriteProvenance, TaskWriteSurface};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::redact_sensitive_env_text;

/// Audit `command` (and `tool_name`) recorded for federated, tool-session, and
/// web task mutations on the owning host.
pub const TASK_MUTATION_AUDIT_COMMAND: &str = "task.mutation";

thread_local! {
    static CURRENT_TASK_WRITE_PROVENANCE: RefCell<Option<TaskWriteProvenance>> =
        const { RefCell::new(None) };
}

struct TaskWriteProvenanceGuard {
    previous: Option<TaskWriteProvenance>,
}

impl Drop for TaskWriteProvenanceGuard {
    fn drop(&mut self) {
        CURRENT_TASK_WRITE_PROVENANCE.with(|slot| {
            *slot.borrow_mut() = self.previous.take();
        });
    }
}

/// Run `operation` with `provenance` active on the current thread so every
/// human-attributed task history event persisted inside `operation` stamps
/// `surface` and `caller`.
pub fn with_task_write_provenance<T>(
    provenance: TaskWriteProvenance,
    operation: impl FnOnce() -> T,
) -> T {
    let previous = CURRENT_TASK_WRITE_PROVENANCE.with(|slot| slot.replace(Some(provenance)));
    let _guard = TaskWriteProvenanceGuard { previous };
    operation()
}

/// Read the active task-write provenance on the current thread, if any.
pub(crate) fn current_task_write_provenance() -> Option<TaskWriteProvenance> {
    CURRENT_TASK_WRITE_PROVENANCE.with(|slot| slot.borrow().clone())
}

/// Classify a task-mutation error into its audit status.
pub fn audit_status_for_task_mutation_error(error: &OrbitError) -> AuditEventStatus {
    if error.is_policy_denial()
        || matches!(
            error,
            OrbitError::PolicyDenied(_)
                | OrbitError::CapabilityDenied(_)
                | OrbitError::CapabilityRefused(_)
        )
    {
        AuditEventStatus::Denied
    } else {
        AuditEventStatus::Failure
    }
}

/// One owner-side `task.mutation` audit record.
pub struct TaskMutationAudit<'a> {
    pub verb: &'a str,
    pub target_id: Option<&'a str>,
    pub role: &'a str,
    pub surface: TaskWriteSurface,
    pub caller: Option<&'a str>,
    pub status: AuditEventStatus,
    pub duration_ms: i64,
    pub error_message: Option<String>,
    pub detail: Value,
    pub session_context: Option<&'a ToolSessionContext>,
}

impl OrbitRuntime {
    /// Resolve the effective actor label recorded on a `task.mutation` row.
    pub fn resolve_task_mutation_audit_role(
        &self,
        agent: Option<&str>,
        model: Option<&str>,
    ) -> String {
        let (canonical_agent, canonical_model) = self
            .try_canonical_agent_model_identity(agent, model)
            .unwrap_or((None, None));
        super::helpers::effective_actor_label(
            &self.actor().label,
            canonical_agent.as_deref(),
            canonical_model.as_deref(),
        )
        .unwrap_or_else(|_| self.actor().audit_role().to_string())
    }

    /// Persist one owner-side `task.mutation` audit row. An audit-store failure
    /// is logged rather than raised so a committed task write is never reported
    /// as failed.
    pub fn record_task_mutation_audit(&self, event: TaskMutationAudit<'_>) {
        let TaskMutationAudit {
            verb,
            target_id,
            role,
            surface,
            caller,
            status,
            duration_ms,
            error_message,
            detail,
            session_context,
        } = event;
        let caller = caller.map(str::trim).filter(|value| !value.is_empty());
        let target_id = target_id.map(str::trim).filter(|value| !value.is_empty());
        let mut payload = json!({
            "surface": surface.as_str(),
            "caller": caller,
            "verb": verb,
        });
        if let (Value::Object(map), Value::Object(extra)) = (&mut payload, detail) {
            map.extend(extra);
        }
        let workspace_id = session_context
            .and_then(|context| context.workspace_id.clone())
            .or_else(|| self.workspace_id().ok());
        let origin_session_id = session_context
            .and_then(|context| context.origin_session_id.clone())
            .or_else(|| caller.map(ToOwned::to_owned));
        let env = self.execution_env_policy().process_env();
        let params = AuditEventInsertParams {
            execution_id: audit_execution_id("task-mutation"),
            command: TASK_MUTATION_AUDIT_COMMAND.to_string(),
            subcommand: Some(verb.to_string()),
            tool_name: Some(TASK_MUTATION_AUDIT_COMMAND.to_string()),
            target_type: Some("task".to_string()),
            target_id: target_id.map(ToOwned::to_owned),
            role: role.to_string(),
            status,
            exit_code: i32::from(status != AuditEventStatus::Success),
            duration_ms: duration_ms.max(0),
            working_directory: self.paths().repo_root.display().to_string(),
            arguments_json: serde_json::to_string(&payload).ok(),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: error_message.map(|message| redact_sensitive_env_text(&message)),
            host: env
                .hostname()
                .and_then(OsStr::to_str)
                .map(ToOwned::to_owned),
            pid: std::process::id(),
            session_id: None,
            workspace_id,
            caller_machine_id: session_context
                .and_then(|context| context.caller_machine_id.clone()),
            caller_machine_name: session_context
                .and_then(|context| context.caller_machine_name.clone()),
            process_machine_id: session_context
                .and_then(|context| context.process_machine_id.clone())
                .or_else(|| self.automation_machine_identity().map(ToOwned::to_owned)),
            process_machine_name: session_context
                .and_then(|context| context.process_machine_name.clone()),
            transport: session_context.and_then(|context| context.transport),
            effective_capabilities: session_context
                .map(|context| context.effective_capabilities.clone())
                .unwrap_or_default(),
            origin_session_id,
            mcp_call_id: session_context.and_then(|context| context.mcp_call_id.clone()),
            lease_id: None,
            task_id: target_id.map(ToOwned::to_owned),
            job_run_id: env
                .orbit_run_id()
                .and_then(OsStr::to_str)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            activity_id: env
                .orbit_activity_id()
                .and_then(OsStr::to_str)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            step_index: env
                .orbit_step_index()
                .and_then(OsStr::to_str)
                .and_then(|value| value.parse().ok()),
        };
        let invocation = || AuditInvocationFields {
            trace_id: session_context.and_then(|context| context.trace_id.as_deref()),
            caller_ip: session_context.and_then(|context| context.caller_ip.as_deref()),
            self_reported_actor: session_context
                .and_then(|context| context.self_reported_actor.as_deref()),
            ..AuditInvocationFields::default()
        };
        let mut write = self.record_audit_event_with_invocation(&params, invocation());
        if status == AuditEventStatus::Success
            && write.as_ref().is_err_and(OrbitError::is_lock_busy)
        {
            write = self.record_audit_event_with_invocation(&params, invocation());
        }
        if let Err(error) = write {
            tracing::warn!(
                target: "orbit.task.audit",
                verb,
                target_id = ?target_id,
                %error,
                "failed to record task.mutation audit row"
            );
        }
    }
}
