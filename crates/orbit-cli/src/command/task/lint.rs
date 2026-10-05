use clap::Args;
use orbit_core::application::task::TaskLintSeverity;
use orbit_core::{OrbitError, OrbitRuntime, TaskStatus};
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

/// Statuses swept when linting without a task ID.
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
    after_help = "Examples:\n  orbit task lint <TASK_ID>                   # findings for one task\n  orbit task lint                             # sweep active tasks for context that needs repair\n  orbit task lint <TASK_ID> --restore-pruned  # re-declare selectors an old prune recorded\n  orbit task lint --restore-pruned            # apply the restoration across the sweep\n  orbit task lint --status review"
)]
pub struct TaskLintArgs {
    /// Task ID. Omit to sweep all active tasks for `context_files` that need repair.
    pub id: Option<String>,
    /// Re-declare `context_files` entries that an earlier prune recorded in task history
    #[arg(long = "restore-pruned")]
    pub restore_pruned: bool,
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
            Some(id) => lint_single_task(runtime, id, self.restore_pruned),
            None => sweep_context_repairs(runtime, self.restore_pruned, &self.statuses),
        }
    }
}

fn lint_single_task(runtime: &OrbitRuntime, id: &str, restore_pruned: bool) -> CommandOut {
    let restoration = if restore_pruned {
        Some(runtime.restore_pruned_context_files(id)?.1)
    } else {
        None
    };

    let report = runtime.lint_task(id)?;
    let mut value = serde_json::to_value(&report).map_err(|e| OrbitError::Io(e.to_string()))?;
    if let (Some(restoration), Value::Object(map)) = (restoration.as_ref(), &mut value) {
        map.insert("restored".to_string(), json!(restoration.restored));
        map.insert("unrestorable".to_string(), json!(restoration.unrestorable));
    }

    let mut lines = Vec::new();
    if let Some(restoration) = restoration.as_ref() {
        if restoration.restored.is_empty() {
            lines.push(format!(
                "No pruned context_files entries recorded in the history of '{}'.",
                report.task_id
            ));
        } else {
            lines.push(format!(
                "Restored {} context_files entr{} on '{}': {}",
                restoration.restored.len(),
                if restoration.restored.len() == 1 {
                    "y"
                } else {
                    "ies"
                },
                report.task_id,
                restoration.restored.join(", ")
            ));
        }
        if !restoration.unrestorable.is_empty() {
            lines.push(format!(
                "Recorded but not restorable against this workspace (repair by hand): {}",
                restoration.unrestorable.join(", ")
            ));
        }
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

/// Sweep active tasks for context declarations that need repair before they
/// can be admitted: an empty or unusable surface, or selectors an earlier
/// prune removed and recorded ([ORB-12490]).
///
/// Nothing here deletes a declaration. The sweep that used to drop selectors
/// whose target had disappeared is retired: a missing target is a valid
/// declaration for work that will create it, and pruning it silently shrank
/// the footprint the task's locks protect.
fn sweep_context_repairs(
    runtime: &OrbitRuntime,
    restore_pruned: bool,
    statuses: &[TaskStatus],
) -> CommandOut {
    let allowed_statuses: &[TaskStatus] = if statuses.is_empty() {
        SWEEP_ACTIVE_STATUSES
    } else {
        statuses
    };

    let tasks = runtime.list_tasks()?;
    let mut report = Vec::<Value>::new();
    let mut total_restored = 0usize;
    let mut total_unrestorable = 0usize;
    let mut tasks_written = 0usize;

    for task in tasks {
        if !allowed_statuses.contains(&task.status) {
            continue;
        }
        let restoration = if restore_pruned {
            runtime.restore_pruned_context_files(&task.id)?.1
        } else {
            runtime.plan_context_file_restore(&task.id)?
        };
        let declared = runtime.declared_context_surface(&task);
        let empty_surface = declared.retained.is_empty();
        if restoration.is_empty() && declared.invalid.is_empty() && !empty_surface {
            continue;
        }

        total_restored += restoration.restored.len();
        total_unrestorable += restoration.unrestorable.len();
        if restoration.applied {
            tasks_written += 1;
        }

        report.push(json!({
            "id": task.id,
            "status": task.status,
            "empty_surface": empty_surface,
            "invalid": declared.invalid,
            "restorable": restoration.restored,
            "unrestorable": restoration.unrestorable,
            "written": restoration.applied,
        }));
    }

    let payload = json!({
        "tasks_needing_repair": report.len(),
        "total_restorable": total_restored,
        "total_unrestorable": total_unrestorable,
        "tasks_written": tasks_written,
        "dry_run": !restore_pruned,
        "tasks": report,
    });

    if report.is_empty() {
        return Ok(Payload::detail(
            payload,
            "No active tasks have context_files that need repair.",
        )
        .into());
    }

    let mut lines = Vec::new();
    for entry in &report {
        let id = entry.get("id").and_then(Value::as_str).unwrap_or("");
        let mut notes = Vec::new();
        if entry.get("empty_surface").and_then(Value::as_bool) == Some(true) {
            notes.push("declares no usable context".to_string());
        }
        for (label, field) in [
            ("invalid", "invalid"),
            (
                if restore_pruned {
                    "restored"
                } else {
                    "restorable"
                },
                "restorable",
            ),
            ("unrestorable", "unrestorable"),
        ] {
            let listed = joined_selectors(entry, field);
            if !listed.is_empty() {
                notes.push(format!("{label}: {listed}"));
            }
        }
        lines.push(format!("{id}: {}", notes.join("; ")));
    }
    let action = if restore_pruned {
        "restored"
    } else {
        "would restore"
    };
    lines.push(format!(
        "\n{action} {total_restored} recorded entr{} across {} task(s).",
        if total_restored == 1 { "y" } else { "ies" },
        report.len()
    ));
    if !restore_pruned {
        lines.push("Re-run with --restore-pruned to apply.".to_string());
    }
    Ok(Payload::detail(payload, lines.join("\n")).into())
}

fn joined_selectors(entry: &Value, field: &str) -> String {
    entry
        .get(field)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}
