//! Out-of-band garbage collection for resources left behind by terminal runs.

use clap::builder::TypedValueParser;
use clap::{Args, Subcommand};
use orbit_core::application::gc::DEFAULT_DEPLOYMENT_KEEP;
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
    /// Reap job-run worktrees whose associated task has settled to rejected, archived, or done,
    /// or whose claim this follower has settled with its owner
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
    /// Delete applied task commit journal rows older than `retention.journal_days`, keeping each task's newest
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

#[derive(Args)]
pub struct TmpGcArgs {
    /// Remove scratch contents; without this flag the command only reports
    #[arg(long, visible_alias = "yes", conflicts_with = "dry_run")]
    pub confirm: bool,

    /// Explicitly request the default non-destructive mode
    #[arg(long, conflicts_with = "confirm")]
    pub dry_run: bool,
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

#[derive(Args)]
#[command(
    after_help = "Without --confirm this only reports. Three classes are counted separately: \
                  top-level entries of ~/.orbit/deployments, copies of replaced executables in \
                  ~/.orbit/bin (names with a .bak, .before-, .pre-, .stale or -stale- marker), \
                  and store copies named ~/.orbit/orbit.db.bak*. Each class keeps its newest \
                  entries by the newest modification time inside them. The installed \
                  executable and every <executable>.previous rollback copy are never removed, \
                  nor is a deployment whose orbit-local-candidate manifest names the bytes of \
                  the installed or rollback executable; those do not count toward --keep. \
                  Links are removed, never followed.\n\nExamples:\n  orbit gc deployments\n  \
                  orbit gc deployments --keep 1 --confirm --json"
)]
pub struct DeploymentGcArgs {
    /// Remove what the report lists; without this flag the command only reports
    #[arg(long, visible_alias = "yes", conflicts_with = "dry_run")]
    pub confirm: bool,

    /// Explicitly request the default non-destructive mode
    #[arg(long, conflicts_with = "confirm")]
    pub dry_run: bool,

    /// Newest entries to keep in each class, besides the rollback-pinned ones
    #[arg(
        long,
        value_name = "N",
        default_value_t = DEFAULT_DEPLOYMENT_KEEP,
        value_parser = clap::value_parser!(u16).range(1..).map(usize::from)
    )]
    pub keep: usize,
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

#[derive(Args)]
#[command(
    after_help = "Without --apply this only reports. The command audit (`audit_events`) is \
                  host-wide. Run audit rows are this workspace's, plus rows whose workspace id is \
                  no longer registered; another registered workspace keeps its own window. Blobs \
                  are this workspace's. A blob is kept while \
                  any remaining run audit row, run step or pipeline state names it, while a \
                  pending-publication marker newer than the cutoff names it, and for 24 hours \
                  after it was last written. Deletes run in batches of 1,000 rows, each its own \
                  write transaction. Freed pages stay in the store file; VACUUM it while Orbit \
                  is idle to return them to the filesystem.\n\nExamples:\n  orbit gc audit\n  \
                  orbit gc audit --older-than-days 30 --apply --json"
)]
pub struct AuditGcArgs {
    /// Delete what the plan lists; without this flag the command only reports
    #[arg(long)]
    pub apply: bool,

    /// Retention window in days, overriding `retention.audit_days` (default 60)
    #[arg(long, value_name = "DAYS")]
    pub older_than_days: Option<u32>,
}

impl Execute for AuditGcArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let report = runtime.gc_audit(self.apply, self.older_than_days)?;
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

#[derive(Args)]
#[command(
    after_help = "Without --apply this only reports. Only success, failed, timeout, cancelled and \
                  interrupted runs of this workspace, and of workspace ids that are no longer \
                  registered, are selected; another registered workspace keeps its own window. A \
                  held run, which review \
                  evidence can still resume, and every non-terminal run are never touched. The \
                  run row, its steps and its summary stay, so `orbit run show` and run history \
                  keep working, and `archived_at` records when the state was dropped. The \
                  host's per-minute run and host resource history, and the admission \
                  forecasts recorded against it, older than 30 days go in the same pass, \
                  whatever the window.\n\n\
                  Examples:\n  orbit gc runs\n  orbit gc runs --apply"
)]
pub struct RunGcArgs {
    /// Drop what the plan lists; without this flag the command only reports
    #[arg(long)]
    pub apply: bool,

    /// Retention window in days, overriding `retention.runs_days` (default 60)
    #[arg(long, value_name = "DAYS")]
    pub older_than_days: Option<u32>,
}

impl Execute for RunGcArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let report = runtime.gc_runs(self.apply, self.older_than_days)?;
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

#[derive(Args)]
#[command(
    after_help = "Without --apply this only reports. Only applied rows of this workspace, and of \
                  workspace ids that are no longer registered, are selected; another registered \
                  workspace keeps its own window. An applied row is a commit decision already \
                  published to its task, so recovery never reads it again. Prepared and \
                  committed rows, which recovery replays, aborted rows, and each task's newest \
                  applied row are never deleted. The rows are selected by one read that holds \
                  no write lock, then deleted by key in batches of 1,000 rows, each its own \
                  write transaction. Freed pages stay in the store file; VACUUM it while Orbit \
                  is idle to return them to the filesystem.\n\n\
                  Examples:\n  orbit gc journal\n  orbit gc journal --apply --json"
)]
pub struct JournalGcArgs {
    /// Delete what the plan lists; without this flag the command only reports
    #[arg(long)]
    pub apply: bool,

    /// Retention window in days, overriding `retention.journal_days` (default 14)
    #[arg(long, value_name = "DAYS")]
    pub older_than_days: Option<u32>,
}

impl Execute for JournalGcArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let report = runtime.gc_journal(self.apply, self.older_than_days)?;
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

/// The mode, window, write batches and store pages shared by the store reports.
fn summary_line(doc: &serde_json::Value) -> String {
    let field = |pointer: &str| {
        doc.pointer(pointer)
            .map(|value| {
                value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_string)
            })
            .unwrap_or_default()
    };
    format!(
        "mode={} retention_days={} cutoff={} batches={} max_batch_ms={} store_bytes={} \
         freelist_bytes={}",
        if doc["apply"] == true {
            "apply"
        } else {
            "plan"
        },
        field("/retention_days"),
        field("/cutoff"),
        field("/writes/batches"),
        field("/writes/max_batch_ms"),
        field("/store/file_bytes"),
        field("/store/freelist_bytes"),
    )
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

    /// Walk eligible worktrees to estimate reclaimable bytes. Dry-run skips
    /// this walk by default; `--confirm` always measures before removal.
    #[arg(long)]
    pub estimate_bytes: bool,

    /// Reclaim worktree.reclaim paths and keep the checkout. Reports bytes
    /// per pattern for terminal runs with no live or undecidable worker;
    /// combine with --confirm to delete. --target-only is an alias.
    #[arg(long, alias = "target-only")]
    pub reclaim: bool,
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
