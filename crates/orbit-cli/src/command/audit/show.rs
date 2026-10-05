use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute, Payload};

use super::support::audit_event_to_json;

#[derive(Args)]
pub struct AuditShowArgs {
    /// Audit event ID
    pub id: i64,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for AuditShowArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let event = runtime.show_audit_event(self.id)?;
        let doc = audit_event_to_json(&event);

        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "ID:                {}", event.id);
        let _ = writeln!(out, "Execution ID:      {}", event.execution_id);
        let _ = writeln!(out, "Timestamp:         {}", event.timestamp.to_rfc3339());
        let _ = writeln!(out, "Command:           {}", event.command);
        let _ = writeln!(
            out,
            "Subcommand:        {}",
            event.subcommand.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Tool:              {}",
            event.tool_name.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Target type:       {}",
            event.target_type.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Target ID:         {}",
            event.target_id.as_deref().unwrap_or("-")
        );
        let _ = writeln!(out, "Role:              {}", event.role);
        // ORB-10890: the trailing marker is not decoration. `Role` is what
        // Orbit authenticated; this line is whatever the caller typed about
        // itself, and reading the two without the distinction is how a
        // forgeable number gets published as a measured one.
        let _ = writeln!(
            out,
            "Self-reported:     {}",
            event
                .self_reported_actor
                .as_deref()
                .map(|actor| format!("{actor} (unverified)"))
                .unwrap_or_else(|| "-".to_string())
        );
        let _ = writeln!(out, "Status:            {}", event.status);
        let _ = writeln!(out, "Exit code:         {}", event.exit_code);
        let _ = writeln!(out, "Duration (ms):     {}", event.duration_ms);
        let _ = writeln!(out, "Working dir:       {}", event.working_directory);
        let _ = writeln!(out, "PID:               {}", event.pid);
        let _ = writeln!(
            out,
            "Host:              {}",
            event.host.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Workspace ID:      {}",
            event.workspace_id.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Caller machine:    {}",
            event.caller_machine_id.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Caller host:       {}",
            event.caller_machine_name.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Process machine:   {}",
            event.process_machine_id.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Process host ID:   {}",
            event.process_machine_name.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Transport:         {}",
            event
                .transport
                .map(|value| value.to_string())
                .as_deref()
                .unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Capabilities:      {}",
            event
                .effective_capabilities
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        let _ = writeln!(
            out,
            "Origin session:    {}",
            event.origin_session_id.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "MCP call:          {}",
            event.mcp_call_id.as_deref().unwrap_or("-")
        );
        let _ = writeln!(
            out,
            "Lease:             {}",
            event.lease_id.as_deref().unwrap_or("-")
        );
        if let Some(ref err) = event.error_message {
            let _ = writeln!(out, "Error:             {err}");
        }
        Ok(Payload::detail(doc, out).into())
    }
}
