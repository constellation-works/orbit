use clap::Args;
use orbit_core::application::task::TaskLintSeverity;
use orbit_core::{OrbitError, OrbitRuntime, TaskStatus};
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

/// Statuses swept when linting without a task ID (the former `prune-context`
/// active set).
///
/// Done / Archived / Rejected tasks are intentionally skipped — they are
/// historical records and re-saving them would mutate audit trails for tasks
/// nobody is going to execute again.
const SWEEP_ACTIVE_STATUSES: &[TaskStatus] = &[
    TaskStatus::Proposed,
    TaskStatus::Backlog,
    TaskStatus::Someday,
    TaskStatus::InProgress,
    TaskStatus::Blocked,
    TaskStatus::Review,
];

#[derive(Args)]
#[command(
    after_help = "Examples:\n  orbit task lint <TASK_ID>            # findings for one task\n  orbit task lint <TASK_ID> --fix      # drop stale context_files entries, then report\n  orbit task lint                      # sweep active tasks for stale context_files (dry run)\n  orbit task lint --fix                # apply the sweep\n  orbit task lint --fix --status review"
)]
pub struct TaskLintArgs {
    /// Task ID. Omit to sweep all active tasks for stale `context_files` entries.
    pub id: Option<String>,
    /// Drop `context_files` entries whose paths no longer exist (formerly
    /// `orbit task prune-context --write`)
    #[arg(long, alias = "write")]
    pub fix: bool,
    /// Restrict the sweep to specific statuses (repeatable; sweep mode only)
    #[arg(long = "status", value_enum, conflicts_with = "id")]
    pub statuses: Vec<TaskStatus>,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for TaskLintArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match &self.id {
            Some(id) => lint_single_task(runtime, id, self.fix),
            None => sweep_stale_context_files(runtime, self.fix, &self.statuses),
        }
    }
}

fn lint_single_task(runtime: &OrbitRuntime, id: &str, fix: bool) -> CommandOut {
    let pruned = if fix {
        let (_task, dropped) = runtime.prune_task_context_files(id)?;
        dropped
    } else {
        Vec::new()
    };

    let report = runtime.lint_task(id)?;
    let mut value = serde_json::to_value(&report).map_err(|e| OrbitError::Io(e.to_string()))?;
    if fix && let Value::Object(map) = &mut value {
        map.insert("pruned".to_string(), json!(pruned));
    }

    let mut lines = Vec::new();
    if !pruned.is_empty() {
        lines.push(format!(
            "Pruned {} stale context_files entr{} from '{}': {}",
            pruned.len(),
            if pruned.len() == 1 { "y" } else { "ies" },
            report.task_id,
            pruned.join(", ")
        ));
    }

    if report.findings.is_empty() {
        lines.push(format!(
            "No lint findings for '{}' ({} ms).",
            report.task_id, report.duration_ms
        ));
        return Ok(Payload::detail(value, lines.join("\n")).into());
    }

    lines.push(format!(
        "{} finding(s) for '{}' ({} ms):",
        report.finding_count, report.task_id, report.duration_ms
    ));
    for finding in report.findings {
        let severity = match finding.severity {
            TaskLintSeverity::Error => "error",
            TaskLintSeverity::Warning => "warning",
        };
        lines.push(format!(
            "[{severity}] {}: {}",
            finding.check, finding.message
        ));
        lines.push(format!("  fix: {}", finding.fix_it));
    }
    Ok(Payload::detail(value, lines.join("\n")).into())
}

fn sweep_stale_context_files(
    runtime: &OrbitRuntime,
    fix: bool,
    statuses: &[TaskStatus],
) -> CommandOut {
    let allowed_statuses: &[TaskStatus] = if statuses.is_empty() {
        SWEEP_ACTIVE_STATUSES
    } else {
        statuses
    };

    let tasks = runtime.list_tasks()?;
    let mut report = Vec::<Value>::new();
    let mut total_dropped = 0usize;
    let mut tasks_with_drops = 0usize;
    let mut tasks_written = 0usize;

    for task in tasks {
        if !allowed_statuses.contains(&task.status) {
            continue;
        }
        if task.context_files.is_empty() {
            continue;
        }
        let dropped = if fix {
            let (_task, dropped) = runtime.prune_task_context_files(&task.id)?;
            dropped
        } else {
            runtime.dry_run_prune_context_files(&task)
        };
        if dropped.is_empty() {
            continue;
        }

        tasks_with_drops += 1;
        total_dropped += dropped.len();
        if fix {
            tasks_written += 1;
        }

        report.push(json!({
            "id": task.id,
            "status": task.status,
            "dropped": dropped,
            "written": fix,
        }));
    }

    let payload = json!({
        "tasks_inspected": report.len(),
        "tasks_with_drops": tasks_with_drops,
        "total_dropped": total_dropped,
        "tasks_written": tasks_written,
        "dry_run": !fix,
        "tasks": report,
    });

    if report.is_empty() {
        return Ok(
            Payload::detail(payload, "No active tasks have stale context_files entries.").into(),
        );
    }

    let mut lines = Vec::new();
    for entry in &report {
        let id = entry.get("id").and_then(Value::as_str).unwrap_or("");
        let dropped = entry
            .get("dropped")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        lines.push(format!("{id}: {dropped}"));
    }
    let action = if fix { "pruned" } else { "would prune" };
    lines.push(format!(
        "\n{action} {total_dropped} entries across {tasks_with_drops} task(s)."
    ));
    if !fix {
        lines.push("Re-run with --fix to apply.".to_string());
    }
    Ok(Payload::detail(payload, lines.join("\n")).into())
}
