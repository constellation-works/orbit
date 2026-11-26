use clap::{Args, Subcommand};
use orbit_core::{OrbitError, OrbitRuntime};

use super::audit::AuditGcArgs;
use super::deployments::DeploymentGcArgs;
use super::journal::JournalGcArgs;
use super::runs::RunGcArgs;
use super::support::summary_line;
use super::tmp::TmpGcArgs;
use super::worktrees::WorktreeGcArgs;
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
    /// Reap job-run worktrees whose associated task has settled to rejected, archived, or done,
    /// or whose claim this follower has settled with its owner. Also reap terminal-run worktrees
    /// for tasks parked in someday for a fixed 336 hours (14 days).
    Worktrees(WorktreeGcArgs),
    /// Reclaim this workspace checkout's scratch contents when no job runs are active
    Tmp(TmpGcArgs),
    /// Remove deployment directories, executable backups and store backups in the Orbit home
    /// beyond the newest of each, always keeping the rollback executable
    Deployments(DeploymentGcArgs),
    /// Prune audit rows older than `retention.audit_days` and the audit blobs no remaining row names
    Audit(AuditGcArgs),
    /// Drop the pipeline state of terminal runs older than `retention.runs_days`, keeping their run and step rows
    Runs(RunGcArgs),
    /// Delete applied task commit journal rows older than 14 days, keeping each task's newest
    Journal(JournalGcArgs),
}

impl Execute for GcTarget {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self {
            Self::Worktrees(args) => args.execute(runtime),
            Self::Tmp(args) => args.execute(runtime),
            Self::Deployments(args) => args.execute(runtime),
            Self::Audit(args) => args.execute(runtime),
            Self::Runs(args) => args.execute(runtime),
            Self::Journal(args) => args.execute(runtime),
        }
    }
}

impl Execute for TmpGcArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let result = runtime.gc_tmp(self.confirm)?;
        let doc = serde_json::to_value(&result).map_err(|error| {
            OrbitError::Execution(format!("failed to serialize tmp GC report: {error}"))
        })?;
        let mut lines: Vec<_> = result
            .reports
            .iter()
            .map(|report| {
                format!(
                    "path={} action={} bytes_reclaimable={} bytes_reclaimed={}",
                    report.path.display(),
                    if result.dry_run {
                        "would_remove"
                    } else {
                        "removed"
                    },
                    report.bytes_reclaimable,
                    report.bytes_reclaimed,
                )
            })
            .collect();
        lines.push(format!(
            "entries_removed={} total_bytes_reclaimable={} total_bytes_reclaimed={}",
            result.entries_removed, result.bytes_reclaimable, result.bytes_reclaimed,
        ));
        Ok(Payload::blocks(doc, vec![Block::text(lines.join("\n"))]).into())
    }
}

impl Execute for DeploymentGcArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let result = runtime.gc_deployments(self.confirm, self.keep)?;
        let doc = serde_json::to_value(&result).map_err(|error| {
            OrbitError::Execution(format!("failed to serialize deployment GC report: {error}"))
        })?;
        let mut lines: Vec<_> = result
            .entries
            .iter()
            .map(|entry| {
                format!(
                    "class={} path={} action={} reason={} bytes={} modified_at={}",
                    entry.class.as_str(),
                    entry.path.display(),
                    entry.action,
                    entry.reason,
                    entry.bytes,
                    entry.modified_at.as_deref().unwrap_or("-"),
                )
            })
            .collect();
        lines.push(format!(
            "mode={} keep={} entries_removed={} total_bytes_reclaimable={} \
             total_bytes_reclaimed={} total_bytes_kept={}",
            if result.dry_run { "plan" } else { "confirm" },
            result.keep,
            result.entries_removed,
            result.bytes_reclaimable,
            result.bytes_reclaimed,
            result.bytes_kept,
        ));
        Ok(Payload::blocks(doc, vec![Block::text(lines.join("\n"))]).into())
    }
}

impl Execute for AuditGcArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let report = runtime.gc_audit(self.confirm, self.older_than_days)?;
        let doc = serde_json::to_value(&report).map_err(|error| {
            OrbitError::Execution(format!("failed to serialize audit GC report: {error}"))
        })?;
        let mut lines: Vec<_> = report
            .tables
            .iter()
            .map(|table| {
                format!(
                    "table={} scope={} rows={} bytes={} rows_removed={}",
                    table.table, table.scope, table.rows, table.bytes, table.rows_removed
                )
            })
            .collect();
        let blobs = &report.blobs;
        lines.push(format!(
            "blobs={} blob_bytes={} unreferenced={} unreferenced_bytes={} pending={} recent={} \
             stale_markers={} removed={} removed_bytes={}",
            blobs.blobs,
            blobs.bytes,
            blobs.unreferenced,
            blobs.unreferenced_bytes,
            blobs.pending,
            blobs.recent,
            blobs.stale_markers,
            blobs.removed,
            blobs.removed_bytes,
        ));
        lines.push(summary_line(&doc));
        Ok(Payload::blocks(doc, vec![Block::text(lines.join("\n"))]).into())
    }
}

impl Execute for RunGcArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let report = runtime.gc_runs(self.confirm, self.older_than_days)?;
        let doc = serde_json::to_value(&report).map_err(|error| {
            OrbitError::Execution(format!("failed to serialize run GC report: {error}"))
        })?;
        let mut lines = vec![format!(
            "table=job_run_states scope=workspace runs={} bytes={} runs_archived={}",
            report.runs, report.state_bytes, report.runs_archived
        )];
        lines.extend(report.resource_minutes.iter().map(|table| {
            format!(
                "table={} scope=host retention_days={} rows={} rows_removed={}",
                table.table, table.retention_days, table.rows, table.rows_removed
            )
        }));
        lines.push(summary_line(&doc));
        Ok(Payload::blocks(doc, vec![Block::text(lines.join("\n"))]).into())
    }
}

impl Execute for JournalGcArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let report = runtime.gc_journal(self.confirm, self.older_than_days)?;
        let doc = serde_json::to_value(&report).map_err(|error| {
            OrbitError::Execution(format!("failed to serialize journal GC report: {error}"))
        })?;
        let lines = [
            format!(
                "table=task_commit_journal scope=workspace rows={} bytes={} rows_removed={} \
                 selection_ms={}",
                report.rows, report.bytes, report.rows_removed, report.selection_ms
            ),
            summary_line(&doc),
        ];
        Ok(Payload::blocks(doc, vec![Block::text(lines.join("\n"))]).into())
    }
}

impl Execute for WorktreeGcArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let result = runtime.gc_worktrees(
            self.confirm,
            self.run,
            self.older_than_hours,
            self.estimate_bytes,
            self.reclaim,
        )?;
        let doc = serde_json::to_value(&result).map_err(|error| {
            OrbitError::Execution(format!("failed to serialize worktree GC report: {error}"))
        })?;
        let mut lines = Vec::with_capacity(result.reports.len() + 1);
        if result.reports.is_empty() {
            lines.push("No worktrees matched.".to_string());
        }
        for report in &result.reports {
            let mut line = format!(
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
            );
            if let Some(bytes) = report.retained_bytes {
                line.push_str(&format!(" retained_bytes={bytes}"));
            }
            if let Some(recovery) = &report.recovery {
                line.push_str(&format!(
                    " recovery={} location={}",
                    recovery.source, recovery.location
                ));
            }
            if let Some(detail) = &report.detail {
                line.push_str(&format!(" detail={detail}"));
            }
            lines.push(line);
            for path in &report.reclaim {
                lines.push(format!(
                    "  path={} pattern={} action={} bytes_reclaimed={}",
                    path.path.display(),
                    path.pattern,
                    path.action,
                    path.bytes_reclaimed
                ));
            }
        }
        if !result.reports.is_empty() {
            lines.push(format!("total_bytes_reclaimed={}", result.bytes_reclaimed));
        }
        Ok(Payload::blocks(doc, vec![Block::text(lines.join("\n"))]).into())
    }
}
