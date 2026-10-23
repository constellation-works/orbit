//! `orbit task lint` rendering: one task's findings, or the context repair
//! sweep when no task ID is given.

use serde_json::{Map, Value};

use crate::command::{CommandOut, Payload};

pub(super) fn render(input: &Map<String, Value>, value: Value) -> CommandOut {
    let restore_pruned = input.get("restore_pruned") == Some(&Value::Bool(true));
    let text = if input.contains_key("id") {
        single_task_text(&value)
    } else {
        sweep_text(&value, restore_pruned)
    };
    Ok(Payload::detail(value, text).into())
}

fn single_task_text(report: &Value) -> String {
    let task_id = string(report, "task_id");
    let duration_ms = report
        .get("duration_ms")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let mut lines = Vec::new();
    // Present only when the run restored.
    if let Some(restored) = report.get("restored") {
        let restored = strings(restored);
        if restored.is_empty() {
            lines.push(format!(
                "No pruned context_files entries recorded in the history of '{task_id}'."
            ));
        } else {
            lines.push(format!(
                "Restored {} context_files entr{} on '{task_id}': {}",
                restored.len(),
                if restored.len() == 1 { "y" } else { "ies" },
                restored.join(", ")
            ));
        }
        let unrestorable = report.get("unrestorable").map(strings).unwrap_or_default();
        if !unrestorable.is_empty() {
            lines.push(format!(
                "Recorded but not restorable against this workspace (repair by hand): {}",
                unrestorable.join(", ")
            ));
        }
    }

    let findings = report
        .get("findings")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    if findings.is_empty() {
        lines.push(format!(
            "No lint findings for '{task_id}' ({duration_ms} ms)."
        ));
        return lines.join("\n");
    }

    lines.push(format!(
        "{} finding(s) for '{task_id}' ({duration_ms} ms):",
        report
            .get("finding_count")
            .and_then(Value::as_u64)
            .unwrap_or_default()
    ));
    for finding in findings {
        lines.push(format!(
            "[{}] {}: {}",
            string(finding, "severity"),
            string(finding, "check"),
            string(finding, "message")
        ));
        lines.push(format!("  fix: {}", string(finding, "fix_it")));
    }
    lines.join("\n")
}

fn sweep_text(payload: &Value, restore_pruned: bool) -> String {
    let tasks = payload
        .get("tasks")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    if tasks.is_empty() {
        return "No active tasks have context_files that need repair.".to_string();
    }

    let mut lines = Vec::new();
    for entry in tasks {
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
            let listed = entry.get(field).map(strings).unwrap_or_default();
            if !listed.is_empty() {
                notes.push(format!("{label}: {}", listed.join(", ")));
            }
        }
        lines.push(format!("{}: {}", string(entry, "id"), notes.join("; ")));
    }
    let total_restored = payload
        .get("total_restorable")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let action = if restore_pruned {
        "restored"
    } else {
        "would restore"
    };
    lines.push(format!(
        "\n{action} {total_restored} recorded entr{} across {} task(s).",
        if total_restored == 1 { "y" } else { "ies" },
        tasks.len()
    ));
    if !restore_pruned {
        lines.push("Re-run with --restore-pruned to apply.".to_string());
    }
    lines.join("\n")
}

fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}
