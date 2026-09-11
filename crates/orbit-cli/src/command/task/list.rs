use clap::{ArgAction, Args};
use orbit_core::{
    DEFAULT_TASK_LIST_LIMIT, ExternalRef, OrbitError, OrbitRuntime, TaskPriority, TaskStatus,
    TaskType,
};
use serde_json::Value;

use crate::command::{CommandOut, Execute, Payload};

use super::output::{TaskTableFilters, task_table, task_to_json, task_to_signal_json};

/// List tasks with optional filters.
///
/// By default (with no `--status`), tasks are listed under a status-aware rule:
/// non-terminal tasks (proposed, backlog, in-progress, review, blocked, someday)
/// are listed first, followed by terminal tasks (done, archived, rejected), each
/// ordered newest first.
#[derive(Args)]
#[command(
    after_help = "Examples:\n  orbit task list\n  orbit task list --limit 100\n  orbit task list --status backlog\n  orbit task list --status in-progress,review\n  orbit task list --type feature\n  orbit task list --priority high\n  orbit task list --parent T12345678-123456\n  orbit task list --ref jira:ENG-1234\n  orbit task list --has-ref jira\n  orbit task list --tag perf --tag bench\n  orbit task list --path src/auth/login.rs\n  orbit task list --json"
)]
pub struct TaskListArgs {
    /// Filter by one or more statuses (comma-separated). Opt-in: with no
    /// `--status`, tasks of every lifecycle status are listed under a
    /// status-aware rule (non-terminal tasks first, then terminal tasks).
    #[arg(long, value_enum, value_delimiter = ',')]
    pub status: Vec<TaskStatus>,
    /// Deprecated no-op: task listing is status-neutral by default, so `--all`
    /// is no longer required to see every lifecycle status. Accepted for
    /// backward compatibility and ignored.
    #[arg(long)]
    pub all: bool,
    /// Maximum number of tasks to return (default 50). Must be at least 1.
    /// Under the default status-aware rule, non-terminal tasks are listed first
    /// (newest first), followed by terminal tasks (newest first).
    #[arg(long, default_value_t = DEFAULT_TASK_LIST_LIMIT, value_parser = parse_task_list_limit)]
    pub limit: usize,
    /// Filter by priority level (low, medium, high)
    #[arg(long, value_enum)]
    pub priority: Option<TaskPriority>,
    /// Filter by task type (feature, bug, refactor, chore)
    #[arg(long = "type", value_enum)]
    pub task_type: Option<TaskType>,
    /// Filter to subtasks belonging to a parent task
    #[arg(long = "parent")]
    pub parent_id: Option<String>,
    /// Filter by job run ID
    #[arg(long)]
    pub job_run_id: Option<String>,
    /// Filter by tag. Repeat for AND semantics.
    #[arg(long = "tag", action = ArgAction::Append, value_delimiter = ',')]
    pub tags: Vec<String>,
    /// Filter by exact external reference in `system:id` form
    #[arg(long = "ref")]
    pub external_ref: Option<String>,
    /// Filter by external reference system
    #[arg(long = "has-ref")]
    pub has_ref: Option<String>,
    /// Keep only tasks whose dependencies are already satisfied
    #[arg(long)]
    pub ready: bool,
    /// Filter to tasks whose `context_files` selectors apply to this path.
    /// Selector forms supported: `file:`, `dir:`, `symbol:`, and bare paths.
    /// Bidirectional containment — passing a directory matches every
    /// selector under it.
    #[arg(long)]
    pub path: Option<String>,
    /// Output full task objects as JSON
    #[arg(long)]
    pub json: bool,
    /// Output signal-tier JSON (id, title, type, status, priority only)
    #[arg(long)]
    pub ops: bool,
    /// Show all table columns in text output
    #[arg(long)]
    pub full: bool,
}

impl Execute for TaskListArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let status = self.status;
        let status_aware_order = status.is_empty();
        let limit = self.limit;
        let priority = self.priority;
        let task_type = self.task_type;
        let parent_id = self.parent_id;
        let job_run_id = self.job_run_id;
        let tags = self.tags;
        let path = self.path;
        let external_ref = self
            .external_ref
            .as_deref()
            .map(ExternalRef::parse_key)
            .transpose()?;
        let has_ref_system = self
            .has_ref
            .map(|system| validate_external_ref_system(&system))
            .transpose()?;
        let ready = self.ready;
        // A column the caller filtered on stays on screen even when the filter
        // made it uniform.
        let filtered = TaskTableFilters {
            status: !status.is_empty(),
            priority: priority.is_some(),
            task_type: task_type.is_some(),
        };

        let page = runtime.query_task_rows_status_aware(
            &orbit_core::application::task::TaskListQuery {
                filter: orbit_core::application::task::TaskListFilter {
                    statuses: Some(status).filter(|statuses| !statuses.is_empty()),
                    priority,
                    task_type,
                    parent_id,
                    job_run_id,
                    tags,
                    external_ref,
                    has_external_ref_system: has_ref_system,
                    scan_before: None,
                    search: None,
                },
                ready,
                path,
                limit,
            },
        )?;
        let total = page.total;
        let status_by_id = page.status_by_id;
        let tasks: Vec<_> = page.items.into_iter().map(|row| row.task).collect();

        if tasks.is_empty() {
            let count = runtime.unindexed_task_bundle_count()?;
            if count > 0 {
                return Err(OrbitError::Store(format!(
                    "task index is missing {count} on-disk bundle(s); run `orbit task reindex` to recover them"
                )));
            }
        }

        let truncated = tasks.len() < total;

        // `--ops` selects a narrower record shape, not a different output
        // channel: the table is the same either way, and the renderer decides
        // whether the caller sees records or rows.
        let records: Vec<Value> = if self.ops {
            tasks.iter().map(task_to_signal_json).collect()
        } else {
            tasks
                .iter()
                .map(|task| task_to_json(task, &status_by_id))
                .collect()
        };

        let mut table = task_table(&tasks, self.full, filtered);
        if truncated {
            // The default listing is status-aware (non-terminal tasks, then
            // terminal), so "newest first" only holds within each bucket, not
            // across the whole result; an explicit `--status` filter runs one
            // query and is newest first throughout (ORB-12200).
            let ordering = if status_aware_order {
                "non-terminal tasks first, then terminal, each newest first"
            } else {
                "newest first"
            };
            table = table.trailing_notice(format!(
                "showing {} of {total} tasks ({ordering}); use --limit N or a filter to see more",
                tasks.len()
            ));
        }

        Ok(Payload::list(records, table).into())
    }
}

fn validate_external_ref_system(system: &str) -> Result<String, OrbitError> {
    ExternalRef::validate_system(system).map_err(Into::into)
}

/// Parse the `--limit` value, rejecting a zero limit (which would return no
/// tasks) with a clear input error (ORB-10310).
fn parse_task_list_limit(raw: &str) -> Result<usize, String> {
    let value: usize = raw
        .parse()
        .map_err(|_| format!("`{raw}` is not a valid limit (expected a positive integer)"))?;
    if value == 0 {
        return Err("limit must be at least 1".to_string());
    }
    Ok(value)
}
