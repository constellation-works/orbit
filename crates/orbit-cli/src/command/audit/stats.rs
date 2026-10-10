use clap::Args;
use orbit_core::{AuditStats, OrbitRuntime};
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};
use crate::parse::parse_since;

#[derive(Args)]
pub struct AuditStatsArgs {
    /// Stats since duration or timestamp
    #[arg(long)]
    pub since: Option<String>,
    /// Filter by tool name
    #[arg(long)]
    pub tool: Option<String>,
}

impl Execute for AuditStatsArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let since = self.since.map(|s| parse_since(&s)).transpose()?;
        let stats = runtime.audit_event_stats(since, self.tool.clone())?;
        let denied_by_operation = scoped_to_tool(
            runtime
                .audit_policy_denial_stats(since.as_ref())?
                .by_operation,
            self.tool.as_deref(),
        );

        let text = format!(
            "Total:             {}\nSuccess:           {}\nFailure:           {}\nDenied:            {}\nAvg duration (ms): {:.1}\nP95 duration (ms): {}\nMax duration (ms): {}{}",
            stats.total,
            stats.success_count,
            stats.failure_count,
            stats.denied_count,
            stats.avg_duration_ms,
            stats.p95_duration_ms,
            stats.max_duration_ms,
            denied_operations_section(&denied_by_operation),
        );
        Ok(Payload::detail(stats_to_json(&stats, &denied_by_operation), text).into())
    }
}

/// Narrow the breakdown to the operation `--tool` selected, so the section
/// describes the same population as the totals above it.
///
/// The authorization row names its operation in `target_id` on every surface,
/// and a governed tool's operation ID *is* its tool name, so an exact match on
/// the filter selects that operation's denials. Without the filter the whole
/// store is in scope.
fn scoped_to_tool(
    mut denied_by_operation: Vec<(String, i64)>,
    tool: Option<&str>,
) -> Vec<(String, i64)> {
    if let Some(tool) = tool {
        denied_by_operation.retain(|(operation, _)| operation == tool);
    }
    denied_by_operation
}

/// The governed-operation denial breakdown, or an empty string when no
/// governed operation was refused in scope.
///
/// Uses the dashboard's canonical capability/policy decisions, including v2
/// policy events. Raw `Denied:` above retains every forensic audit row, so it
/// also includes coordination/protocol refusals and duplicate entry-point rows.
fn denied_operations_section(denied_by_operation: &[(String, i64)]) -> String {
    use std::fmt::Write as _;

    let mut section = String::new();
    if denied_by_operation.is_empty() {
        return section;
    }

    let _ = write!(section, "\nDenied governed operations:");
    for (operation, count) in denied_by_operation {
        let _ = write!(section, "\n  {count:>6}  {operation}");
    }
    section
}

fn stats_to_json(stats: &AuditStats, denied_by_operation: &[(String, i64)]) -> Value {
    json!({
        "total": stats.total,
        "success_count": stats.success_count,
        "failure_count": stats.failure_count,
        "denied_count": stats.denied_count,
        "policy_denied_count": denied_by_operation.iter().map(|(_, count)| count).sum::<i64>(),
        "avg_duration_ms": stats.avg_duration_ms,
        "p95_duration_ms": stats.p95_duration_ms,
        "max_duration_ms": stats.max_duration_ms,
        "denied_by_operation": denied_by_operation
            .iter()
            .map(|(operation, count)| json!({ "operation": operation, "count": count }))
            .collect::<Vec<_>>(),
    })
}
