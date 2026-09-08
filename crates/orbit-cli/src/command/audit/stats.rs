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
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for AuditStatsArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let since = self.since.map(|s| parse_since(&s)).transpose()?;
        let stats = runtime.audit_event_stats(since, self.tool)?;

        let text = format!(
            "Total:             {}\nSuccess:           {}\nFailure:           {}\nDenied:            {}\nAvg duration (ms): {:.1}\nP95 duration (ms): {}\nMax duration (ms): {}",
            stats.total,
            stats.success_count,
            stats.failure_count,
            stats.denied_count,
            stats.avg_duration_ms,
            stats.p95_duration_ms,
            stats.max_duration_ms
        );
        Ok(Payload::detail(stats_to_json(&stats), text).into())
    }
}

fn stats_to_json(stats: &AuditStats) -> Value {
    json!({
        "total": stats.total,
        "success_count": stats.success_count,
        "failure_count": stats.failure_count,
        "denied_count": stats.denied_count,
        "avg_duration_ms": stats.avg_duration_ms,
        "p95_duration_ms": stats.p95_duration_ms,
        "max_duration_ms": stats.max_duration_ms,
    })
}
