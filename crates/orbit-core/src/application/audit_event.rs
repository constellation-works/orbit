use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_store::contracts::{
    AuditActorAggregate, AuditAttributionAggregate, AuditEventFilter, AuditEventInsertParams,
    AuditRoleAggregate, AuditToolAggregate, AuditToolCallCountsByRole,
    AuditToolCallCountsBySurfaceAndRole, AuditTopToolCall, FailureIncidentQuery,
    FailureIncidentReport,
};
use orbit_types::telemetry::{AuditEvent, AuditEventStatus, AuditStats};

use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Returns persistent audit events recorded by the CLI middleware across all invocations.
    /// Backed by SQLite; survives process restarts. For in-process session events only, see
    /// [`OrbitRuntime::list_session_events`].
    pub fn list_audit_events(
        &self,
        since: Option<DateTime<Utc>>,
        tool: Option<String>,
        status: Option<AuditEventStatus>,
        role: Option<String>,
        limit: usize,
    ) -> Result<Vec<AuditEvent>, OrbitError> {
        self.list_audit_events_with_kind(since, tool, None, status, role, limit)
    }

    pub fn list_audit_events_with_kind(
        &self,
        since: Option<DateTime<Utc>>,
        tool: Option<String>,
        target_type: Option<String>,
        status: Option<AuditEventStatus>,
        role: Option<String>,
        limit: usize,
    ) -> Result<Vec<AuditEvent>, OrbitError> {
        self.stores()
            .audit_events()
            .list_audit_events(&AuditEventFilter {
                since,
                tool_name: tool,
                target_type,
                status,
                role,
                workspace_id: None,
                caller_machine_id: None,
                process_machine_id: None,
                transport: None,
                capability: None,
                origin_session_id: None,
                mcp_call_id: None,
                job_run_id: None,
                lease_id: None,
                limit,
                offset: 0,
            })
    }

    pub fn list_audit_events_filtered(
        &self,
        filter: &AuditEventFilter,
    ) -> Result<Vec<AuditEvent>, OrbitError> {
        self.stores().audit_events().list_audit_events(filter)
    }

    pub fn show_audit_event(&self, id: i64) -> Result<AuditEvent, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_event(id)?
            .ok_or_else(|| OrbitError::InvalidInput(format!("audit event not found: {id}")))
    }

    pub fn prune_audit_events(&self, older_than: &DateTime<Utc>) -> Result<usize, OrbitError> {
        self.stores().audit_events().prune_audit_events(older_than)
    }

    pub fn audit_event_stats(
        &self,
        since: Option<DateTime<Utc>>,
        tool: Option<String>,
    ) -> Result<AuditStats, OrbitError> {
        let (total, success_count, failure_count, denied_count, avg_duration_ms, max_duration_ms) =
            self.stores()
                .audit_events()
                .get_audit_event_stats(since.as_ref(), tool.as_deref())?;

        let durations = self
            .stores()
            .audit_events()
            .get_audit_event_durations(since.as_ref(), tool.as_deref())?;

        let p95_duration_ms = compute_p95(&durations);

        Ok(AuditStats {
            total,
            success_count,
            failure_count,
            denied_count,
            avg_duration_ms,
            p95_duration_ms,
            max_duration_ms,
        })
    }

    pub fn record_audit_event(&self, params: &AuditEventInsertParams) -> Result<(), OrbitError> {
        self.stores()
            .audit_events()
            .insert_audit_event_record(params)
    }

    pub fn record_id_allocation_audit(&self, kind: &str, id: &str) -> Result<(), OrbitError> {
        let worktree_root = self
            .paths()
            .local_dir
            .as_path()
            .parent()
            .map(|path| path.to_path_buf())
            .unwrap_or_else(|| self.paths().local_dir.clone());
        let payload = serde_json::json!({
            "kind": kind,
            "id": id,
            "worktree_root": worktree_root.to_string_lossy(),
        });
        let arguments_json = serde_json::to_string(&payload).map_err(|error| {
            OrbitError::Execution(format!("serialize id allocation audit: {error}"))
        })?;
        self.record_audit_event(&AuditEventInsertParams {
            execution_id: audit_execution_id("audit-id-allocation"),
            command: "id".to_string(),
            subcommand: Some("allocate".to_string()),
            tool_name: Some(format!("orbit.{kind}.add")),
            target_type: Some("id_allocation".to_string()),
            target_id: Some(id.to_string()),
            role: "admin".to_string(),
            status: AuditEventStatus::Success,
            exit_code: 0,
            duration_ms: 0,
            working_directory: self.paths().repo_root.to_string_lossy().into_owned(),
            arguments_json: Some(arguments_json),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: None,
            host: std::env::var("HOSTNAME").ok(),
            pid: std::process::id(),
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_host_id: None,
            process_machine_id: None,
            process_host_id: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: None,
            job_run_id: std::env::var("ORBIT_RUN_ID").ok().filter(|s| !s.is_empty()),
            activity_id: std::env::var("ORBIT_ACTIVITY_ID")
                .ok()
                .filter(|s| !s.is_empty()),
            step_index: std::env::var("ORBIT_STEP_INDEX")
                .ok()
                .and_then(|s| s.parse().ok()),
        })
    }

    /// Hourly count buckets `(rfc3339_hour_start, count)` for audit events at or
    /// after `since`. Empty hours are NOT in the result; callers must zero-fill
    /// when rendering a sparkline that needs every bucket.
    pub fn audit_event_hourly_buckets(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<(String, i64)>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_event_hourly_buckets(since)
    }

    /// `(role, count)` for `status='denied'` audit events at or after `since`,
    /// sorted desc by count. When `since` is `None`, all-time totals are returned.
    pub fn audit_denials_by_role(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<(String, i64)>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_denials_by_role(since)
    }

    /// `(operation, count)` for `status='denied'` audit events at or after
    /// `since`, sorted desc by count. Backs `orbit audit stats`'s denial
    /// breakdown [ORB-12257].
    pub fn audit_denials_by_operation(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<(String, i64)>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_denials_by_operation(since)
    }

    /// Per-role counts for audited tool invocations. `failed` counts every
    /// non-success tool run (`failure` and `denied` statuses).
    pub fn audit_tool_call_counts_by_role(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<AuditToolCallCountsByRole>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_tool_call_counts_by_role(since)
    }

    /// Per-(surface, role) tool-call counts where `tool_name` matches
    /// `orbit.<surface>.<verb>`. Drives the public scoreboard's
    /// per-Orbit-surface usage sections.
    pub fn audit_tool_call_counts_by_surface_and_role(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<AuditToolCallCountsBySurfaceAndRole>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_tool_call_counts_by_surface_and_role(since)
    }

    /// The same tool-call rows as [`Self::audit_tool_call_counts_by_role`],
    /// split by how each row's identity was established [ORB-10890].
    ///
    /// Authenticated, self-reported, and anonymous traffic are disjoint
    /// buckets carrying their own `total`/`failed`, so a per-agent fail rate
    /// can name the denominator it was computed over instead of quietly
    /// dropping the ~92% of MCP calls Orbit cannot authenticate.
    pub fn audit_tool_call_counts_by_attribution(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<AuditAttributionAggregate>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_tool_call_counts_by_attribution(since)
    }

    /// Top (role, tool_name) pairs from the audit log, restricted to
    /// `orbit.*` tool names. `limit` caps the result count after sorting.
    pub fn audit_top_tool_calls(
        &self,
        since: Option<&DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<AuditTopToolCall>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_top_tool_calls(since, limit)
    }

    /// Per-tool aggregate of audit events at or after `since`. One row per
    /// distinct `tool_name` (NULL folded into `"unknown"`), each carrying
    /// `total`, `failures`, MCP/CLI surface counts, and `avg_duration_ms`.
    /// Backs the audit-summary side panel.
    pub fn audit_event_aggregates_by_tool(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditToolAggregate>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_event_aggregates_by_tool(since)
    }

    /// Per-role aggregate of audit events at or after `since`, with the
    /// MCP-vs-CLI surface split (`mcp` = `subcommand='run-mcp'`, `cli` =
    /// `subcommand='run'`). Drives the Role-split and MCP-vs-CLI cards.
    pub fn audit_event_aggregates_by_role(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditRoleAggregate>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_event_aggregates_by_role(since)
    }

    /// Per-canonical-actor aggregate of audit events at or after `since`
    /// [ORB-10888].
    ///
    /// Same window and same MCP-vs-CLI split as
    /// [`Self::audit_event_aggregates_by_role`], but grouped on the canonical
    /// actor: one agent recorded as `claude`, `opus`, and `claude-opus-5` is a
    /// single row, and `kind` separates synthetic and human actors from real
    /// agents without matching on the label.
    pub fn audit_event_aggregates_by_actor(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditActorAggregate>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_event_aggregates_by_actor(since)
    }

    /// Failure incidents grouped from the raw failed/denied audit rows in
    /// `query`'s window [ORB-10871]. The report carries both the incident
    /// list and the raw event denominators it was derived from, so a caller
    /// never has to infer one count from the other. Raw rows are neither
    /// mutated nor withheld — `list_audit_events_filtered` remains the
    /// authority on evidence.
    pub fn audit_failure_incidents(
        &self,
        query: &FailureIncidentQuery,
    ) -> Result<FailureIncidentReport, OrbitError> {
        self.stores().audit_events().get_failure_incidents(query)
    }

    /// Sorted `duration_ms` values for audit events with NULL `tool_name`
    /// at or after `since`. Companion to [`Self::audit_event_aggregates_by_tool`]
    /// for computing percentiles of the synthetic `"unknown"` bucket.
    pub fn audit_event_durations_null_tool(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<i64>, OrbitError> {
        self.stores()
            .audit_events()
            .get_audit_event_durations_null_tool(since)
    }
}

/// Nearest-rank p95 over a slice of `duration_ms` values that the caller has
/// already sorted ascending. Returns 0 for an empty slice. Shared with
/// callers that fetch `audit_event_durations_*` directly.
pub fn compute_p95(sorted_durations: &[i64]) -> i64 {
    if sorted_durations.is_empty() {
        return 0;
    }
    let idx = ((sorted_durations.len() as f64) * 0.95).ceil() as usize;
    let idx = idx.min(sorted_durations.len()) - 1;
    sorted_durations[idx]
}
