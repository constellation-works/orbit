//! Audit-event inserts, on the Store connection or inside a caller's
//! transaction.

use orbit_common::OrbitError;
use orbit_types::telemetry::canonical_actor_for_role_label;

use crate::contracts::{AuditEventInsertParams, AuditInvocationFields};
use crate::{Store, StoreTx, now_string};

impl Store {
    pub fn insert_audit_event_record(
        &self,
        params: &AuditEventInsertParams,
    ) -> Result<(), OrbitError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;

        insert_audit_event_record_on_connection(&conn, params, AuditInvocationFields::default())
    }

    /// Insert a canonical audit row with the transport-neutral invocation
    /// fields carried by a tool session. The legacy insert DTO remains stable
    /// for non-tool producers; new invocation producers use this focused seam.
    pub fn insert_audit_event_record_with_invocation(
        &self,
        params: &AuditEventInsertParams,
        invocation: AuditInvocationFields<'_>,
    ) -> Result<(), OrbitError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;

        insert_audit_event_record_on_connection(&conn, params, invocation)
    }
}

impl StoreTx<'_> {
    /// Insert one canonical audit row inside the caller's existing Store
    /// transaction. Vertical features use this when their domain mutation and
    /// its audit outcome must commit or roll back together.
    pub fn insert_audit_event_record(
        &mut self,
        params: &AuditEventInsertParams,
    ) -> Result<(), OrbitError> {
        insert_audit_event_record_on_connection(
            self.connection(),
            params,
            AuditInvocationFields::default(),
        )
    }
}

fn insert_audit_event_record_on_connection(
    conn: &rusqlite::Connection,
    params: &AuditEventInsertParams,
    invocation: AuditInvocationFields<'_>,
) -> Result<(), OrbitError> {
    let capabilities_json = serde_json::to_string(&params.effective_capabilities)
        .map_err(|error| OrbitError::Store(format!("serialize MCP capability set: {error}")))?;
    // ORB-10888: the canonical actor is derived from the same label the row
    // stores, by the same alias map the backfill uses, so new rows and
    // migrated rows land in identical aggregate buckets.
    //
    // ORB-10890: `invocation.self_reported_actor` is deliberately NOT an input
    // here. The trusted projection is a function of `role` alone, so a caller's
    // claim cannot reach any column an authorization or trust decision reads.
    let actor = canonical_actor_for_role_label(&params.role);

    conn.execute(
        r#"INSERT INTO audit_events(
            execution_id, timestamp, command, subcommand, tool_name,
            target_type, target_id, role, status, exit_code,
            duration_ms, working_directory, arguments_json,
            stdout_truncated, stderr_truncated, error_message,
            host, pid, session_id, workspace_id, caller_machine_id,
            caller_machine_name, process_machine_id, process_machine_name, transport,
            capabilities_json, origin_session_id, mcp_call_id, lease_id,
            task_id, job_run_id, activity_id, step_index, trace_id, caller_ip,
            actor_kind, actor_id, actor_vendor, actor_family, actor_model,
            actor_alias_version, self_reported_actor,
            plugin_name, plugin_version, plugin_manifest_digest, plugin_grants
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42, ?43, ?44, ?45, ?46)"#,
        rusqlite::params![
            params.execution_id,
            now_string(),
            params.command,
            params.subcommand,
            params.tool_name,
            params.target_type,
            params.target_id,
            params.role,
            params.status.to_string(),
            params.exit_code,
            params.duration_ms,
            params.working_directory,
            params.arguments_json,
            params.stdout_truncated,
            params.stderr_truncated,
            params.error_message,
            params.host,
            params.pid,
            params.session_id,
            params.workspace_id,
            params.caller_machine_id,
            params.caller_machine_name,
            params.process_machine_id,
            params.process_machine_name,
            params.transport.map(|transport| transport.to_string()),
            capabilities_json,
            params.origin_session_id,
            params.mcp_call_id,
            params.lease_id,
            params.task_id,
            params.job_run_id,
            params.activity_id,
            params.step_index,
            invocation.trace_id,
            invocation.caller_ip,
            actor.kind.as_str(),
            actor.id,
            actor.vendor,
            actor.family,
            actor.model,
            actor.alias_version,
            invocation.self_reported_actor,
            invocation.plugin.map(|plugin| plugin.name.as_str()),
            invocation.plugin.map(|plugin| plugin.version.as_str()),
            invocation.plugin.map(|plugin| plugin.manifest_digest.as_str()),
            invocation
                .plugin
                .map(|plugin| serde_json::to_string(&plugin.grants).unwrap_or_default()),
        ],
    )
        .map_err(|e| OrbitError::Store(e.to_string()))?;

    Ok(())
}
