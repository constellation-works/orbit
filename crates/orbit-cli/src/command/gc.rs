//! Out-of-band garbage collection for resources left behind by terminal runs.

use clap::{Args, Subcommand};
use orbit_core::{OrbitError, OrbitRuntime};

use crate::command::{Block, CommandOut, Execute, Payload};

#[derive(Args)]
#[command(about = "Inspect and explicitly reap Orbit-managed garbage")]
pub struct GcCommand {
    #[command(subcommand)]
    pub target: GcTarget,
}

impl Execute for GcCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        self.target.execute(runtime)
    }
}

/// Garbage classes. New classes extend this enum without changing the command
/// or execution contract.
#[derive(Subcommand)]
pub enum GcTarget {
    /// Reap job-run worktrees whose associated task has settled to rejected, archived, or done
    Worktrees(WorktreeGcArgs),
}

impl Execute for GcTarget {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self {
            Self::Worktrees(args) => args.execute(runtime),
        }
    }
}

#[derive(Args)]
pub struct WorktreeGcArgs {
    /// Perform removals; without this flag the command only reports
    #[arg(long, visible_alias = "yes", conflicts_with = "dry_run")]
    pub confirm: bool,

    /// Explicitly request the default non-destructive mode
    #[arg(long, conflicts_with = "confirm")]
    pub dry_run: bool,

    /// Restrict collection to one job run
    #[arg(long, value_name = "ID")]
    pub run: Option<String>,

    /// Restrict collection to runs finished at least this many hours ago
    #[arg(long, value_name = "HOURS")]
    pub older_than_hours: Option<u64>,

    /// Emit the complete report as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for WorktreeGcArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let result = runtime.gc_worktrees(self.confirm, self.run, self.older_than_hours)?;
        let doc = serde_json::to_value(&result).map_err(|error| {
            OrbitError::Execution(format!("failed to serialize worktree GC report: {error}"))
        })?;
        let mut lines = Vec::with_capacity(result.reports.len() + 1);
        if result.reports.is_empty() {
            lines.push("No worktrees matched.".to_string());
        }
        for report in &result.reports {
            lines.push(format!(
                "path={} run_id={} run_state={} task_id={} task_status={} pr_status={} action={} bytes_reclaimed={}",
                report.path.display(),
                report.run_id.as_deref().unwrap_or("-"),
                report
                    .run_state
                    .map(|state| state.to_string())
                    .as_deref()
                    .unwrap_or("-"),
                report.task_id.as_deref().unwrap_or("-"),
                report
                    .task_status
                    .map(|status| status.to_string())
                    .as_deref()
                    .unwrap_or("-"),
                report.pr_status.as_deref().unwrap_or("-"),
                report.action,
                report.bytes_reclaimed
            ));
        }
        if !result.reports.is_empty() {
            lines.push(format!("total_bytes_reclaimed={}", result.bytes_reclaimed));
        }
        Ok(Payload::blocks(doc, vec![Block::text(lines.join("\n"))]).into())
    }
}
