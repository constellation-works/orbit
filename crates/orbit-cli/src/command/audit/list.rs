use clap::Args;
use orbit_core::{AuditEventFilter, AuditEventStatus, OrbitRuntime};
use orbit_types::tool::{McpCapability, McpTransport};
use serde_json::Value;

use crate::command::{CommandOut, Execute, Payload};
use crate::parse::parse_since;

use super::support::{AuditListFilters, audit_event_table, audit_event_to_json};

#[derive(Args)]
pub struct AuditListArgs {
    /// Filter events since duration or timestamp (e.g. "1h", "90d", RFC3339)
    #[arg(long)]
    pub since: Option<String>,
    /// Filter by tool name
    #[arg(long)]
    pub tool: Option<String>,
    /// Filter by event kind (alias for target_type)
    #[arg(long)]
    pub kind: Option<String>,
    /// Filter by status
    #[arg(long)]
    pub status: Option<AuditEventStatus>,
    /// Filter by role
    #[arg(long)]
    pub role: Option<String>,
    /// Filter by trusted stored workspace ID
    ///
    /// This is deliberately distinct from the global `--workspace` selector:
    /// the latter accepts a registered name or checkout path and is resolved
    /// during runtime bootstrap. The audit log is host-global, so listing is
    /// unscoped unless this filter is passed.
    #[arg(long = "workspace-id")]
    pub workspace_id: Option<String>,
    /// Filter by trusted caller machine ID
    #[arg(long)]
    pub caller_machine: Option<String>,
    /// Filter by trusted executing-process machine ID
    #[arg(long)]
    pub process_machine: Option<String>,
    /// Filter by MCP transport (`local` or `ssh-mcp`)
    #[arg(long)]
    pub transport: Option<McpTransport>,
    /// Filter by effective MCP capability membership
    #[arg(long)]
    pub capability: Option<McpCapability>,
    /// Filter by originating MCP session ID
    #[arg(long)]
    pub origin_session: Option<String>,
    /// Filter by unique MCP call ID
    #[arg(long)]
    pub mcp_call: Option<String>,
    /// Filter by canonical job run ID
    #[arg(long)]
    pub run: Option<String>,
    /// Filter by leased-run lease ID
    #[arg(long)]
    pub lease: Option<String>,
    /// Maximum number of events to return
    #[arg(long, default_value_t = 100)]
    pub limit: usize,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for AuditListArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let since = self.since.map(|s| parse_since(&s)).transpose()?;
        // Captured before the filter consumes them: a column the caller
        // filtered on stays on screen even though it now reads uniform.
        let filtered = AuditListFilters {
            status: self.status.is_some(),
            role: self.role.is_some(),
            tool: self.tool.is_some(),
        };
        let events = runtime.list_audit_events_filtered(&AuditEventFilter {
            since,
            tool_name: self.tool,
            target_type: self.kind,
            status: self.status,
            role: self.role,
            // Only the explicit flag scopes this. The audit log is one
            // host-global table, and a row written before (or without)
            // workspace resolution stores a NULL `workspace_id` — the
            // global-seam denial for an unknown or unadvertised MCP tool name,
            // and workspace setup failures. `workspace_id = ?` cannot match
            // NULL, so defaulting the filter to the bound workspace silently
            // drops exactly the rows an operator audits for.
            workspace_id: self.workspace_id,
            caller_machine_id: self.caller_machine,
            process_machine_id: self.process_machine,
            transport: self.transport,
            capability: self.capability,
            origin_session_id: self.origin_session,
            mcp_call_id: self.mcp_call,
            job_run_id: self.run,
            lease_id: self.lease,
            limit: self.limit,
            offset: 0,
        })?;

        let values: Vec<Value> = events.iter().map(audit_event_to_json).collect();
        Ok(Payload::list(values, audit_event_table(&events, filtered)).into())
    }
}
