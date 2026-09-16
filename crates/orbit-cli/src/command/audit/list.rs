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
    /// during runtime bootstrap.
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
        let workspace_id = self.workspace_id.or_else(|| {
            runtime
                .workspace_runtime_binding()
                .map(|binding| binding.task_partition_id.clone())
        });
        let events = runtime.list_audit_events_filtered(&AuditEventFilter {
            since,
            tool_name: self.tool,
            target_type: self.kind,
            status: self.status,
            role: self.role,
            workspace_id,
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::command::{Cli, Commands, audit::AuditSubcommand};

    #[test]
    fn global_workspace_selector_does_not_populate_audit_workspace_id_filter() {
        for selector in ["qa-10484", "ws_qa-10484"] {
            let cli = Cli::try_parse_from(["orbit", "--workspace", selector, "audit", "list"])
                .expect("global workspace selector should parse");
            assert_eq!(cli.workspace.as_deref(), Some(selector));

            let Commands::Audit(command) = cli.command else {
                panic!("expected audit command");
            };
            let AuditSubcommand::List(args) = command.command else {
                panic!("expected audit list command");
            };
            assert_eq!(args.workspace_id, None);
        }
    }

    #[test]
    fn audit_workspace_id_filter_has_a_distinct_flag() {
        let cli = Cli::try_parse_from(["orbit", "audit", "list", "--workspace-id", "ws_qa-10484"])
            .expect("explicit audit workspace ID should parse");

        let Commands::Audit(command) = cli.command else {
            panic!("expected audit command");
        };
        let AuditSubcommand::List(args) = command.command else {
            panic!("expected audit list command");
        };
        assert_eq!(args.workspace_id.as_deref(), Some("ws_qa-10484"));
    }
}
