use std::io::Write;

use clap::{Args, ValueEnum};
use orbit_core::{AuditEvent, OrbitError, OrbitRuntime};
use serde_json::Value;

use crate::command::{CommandOut, CommandOutput, Execute};
use crate::parse::parse_since;

use super::support::audit_event_to_json;

#[derive(Clone, ValueEnum)]
pub enum ExportFormat {
    Json,
    Csv,
}

#[derive(Args)]
#[command(
    after_help = "`--format` here names the export file's serialization (json or csv) and shadows the global `--format` (auto|table|json|ndjson), which this command does not accept: the export is written to --output, not to stdout."
)]
pub struct AuditExportArgs {
    /// Export file format. Local to this command — it shadows the global
    /// `--format` output-mode argument, which is not accepted here.
    #[arg(long, default_value = "json")]
    pub format: ExportFormat,
    /// Output file path
    #[arg(long)]
    pub output: String,
    /// Filter events since duration or timestamp
    #[arg(long)]
    pub since: Option<String>,
    /// Filter by tool name
    #[arg(long)]
    pub tool: Option<String>,
}

impl Execute for AuditExportArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let since = self.since.map(|s| parse_since(&s)).transpose()?;
        let events = runtime.list_audit_events(since, self.tool, None, None, 0)?;

        match self.format {
            ExportFormat::Json => {
                export_json(&self.output, &events)?;
                Ok(CommandOutput::Silent)
            }
            ExportFormat::Csv => {
                export_csv(&self.output, &events)?;
                Ok(CommandOutput::Silent)
            }
        }
    }
}

fn export_json(path: &str, events: &[AuditEvent]) -> Result<(), OrbitError> {
    let file =
        std::fs::File::create(path).map_err(|e| OrbitError::Io(format!("create {path}: {e}")))?;
    let mut writer = std::io::BufWriter::new(file);

    let values: Vec<Value> = events.iter().map(audit_event_to_json).collect();
    let json_bytes = serde_json::to_string_pretty(&Value::Array(values))
        .map_err(|e| OrbitError::Execution(e.to_string()))?;

    writer
        .write_all(json_bytes.as_bytes())
        .map_err(|e| OrbitError::Io(format!("write {path}: {e}")))?;
    writer
        .write_all(b"\n")
        .map_err(|e| OrbitError::Io(format!("write {path}: {e}")))?;

    println!("Exported {} events to {path}", events.len());
    Ok(())
}

fn export_csv(path: &str, events: &[AuditEvent]) -> Result<(), OrbitError> {
    let mut writer =
        csv::Writer::from_path(path).map_err(|e| OrbitError::Io(format!("create {path}: {e}")))?;

    writer
        .write_record([
            "id",
            "execution_id",
            "timestamp",
            "command",
            "subcommand",
            "tool_name",
            "target_type",
            "target_id",
            "role",
            "status",
            "exit_code",
            "duration_ms",
            "working_directory",
            "arguments_json",
            "stdout_truncated",
            "stderr_truncated",
            "error_message",
            "host",
            "pid",
            "session_id",
            "workspace_id",
            "caller_machine_id",
            "caller_host_id",
            "process_machine_id",
            "process_host_id",
            "transport",
            "effective_capabilities",
            "origin_session_id",
            "mcp_call_id",
            "trace_id",
            "caller_ip",
            "lease_id",
            "task_id",
            "job_run_id",
            "activity_id",
            "step_index",
        ])
        .map_err(|e| OrbitError::Io(format!("write csv header: {e}")))?;

    for event in events {
        writer
            .write_record([
                event.id.to_string(),
                event.execution_id.clone(),
                event.timestamp.to_rfc3339(),
                event.command.clone(),
                event.subcommand.clone().unwrap_or_default(),
                event.tool_name.clone().unwrap_or_default(),
                event.target_type.clone().unwrap_or_default(),
                event.target_id.clone().unwrap_or_default(),
                event.role.clone(),
                event.status.to_string(),
                event.exit_code.to_string(),
                event.duration_ms.to_string(),
                event.working_directory.clone(),
                event.arguments_json.clone().unwrap_or_default(),
                event.stdout_truncated.clone().unwrap_or_default(),
                event.stderr_truncated.clone().unwrap_or_default(),
                event.error_message.clone().unwrap_or_default(),
                event.host.clone().unwrap_or_default(),
                event.pid.to_string(),
                event.session_id.clone().unwrap_or_default(),
                event.workspace_id.clone().unwrap_or_default(),
                event.caller_machine_id.clone().unwrap_or_default(),
                event.caller_host_id.clone().unwrap_or_default(),
                event.process_machine_id.clone().unwrap_or_default(),
                event.process_host_id.clone().unwrap_or_default(),
                event
                    .transport
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                event
                    .effective_capabilities
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("|"),
                event.origin_session_id.clone().unwrap_or_default(),
                event.mcp_call_id.clone().unwrap_or_default(),
                event.trace_id.clone().unwrap_or_default(),
                event.caller_ip.clone().unwrap_or_default(),
                event.lease_id.clone().unwrap_or_default(),
                event.task_id.clone().unwrap_or_default(),
                event.job_run_id.clone().unwrap_or_default(),
                event.activity_id.clone().unwrap_or_default(),
                event
                    .step_index
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ])
            .map_err(|e| OrbitError::Io(format!("write csv row: {e}")))?;
    }

    writer
        .flush()
        .map_err(|e| OrbitError::Io(format!("flush csv: {e}")))?;

    println!("Exported {} events to {path}", events.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_header_appends_trusted_mcp_provenance_columns() {
        let file = tempfile::NamedTempFile::new().expect("temporary CSV");
        let path = file.path().to_str().expect("UTF-8 temp path");
        export_csv(path, &[]).expect("export empty CSV");

        let csv = std::fs::read_to_string(path).expect("read exported CSV");
        let header = csv.lines().next().expect("CSV header");
        assert!(header.contains("host,pid,session_id,workspace_id,caller_machine_id"));
        assert!(header.contains("process_host_id,transport,effective_capabilities"));
        assert!(header.contains("origin_session_id,mcp_call_id,trace_id,caller_ip,lease_id"));
        assert!(header.ends_with("task_id,job_run_id,activity_id,step_index"));
    }
}
