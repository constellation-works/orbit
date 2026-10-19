//! `orbit task show` rendering, from the `orbit.task.show` answer.
//!
//! The record, its sidecars and its operator guidance (`next_step`,
//! `final_recovery`) all arrive in the tool's JSON. Two things are added here
//! for the human reader: the workspace the task was read from, and the status
//! of each relation target, a presentation lookup the tool's `relations`
//! projection does not carry.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use orbit_cmd::task_owner::bound_workspace_identity;
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::task::{TaskComment, TaskHistoryEntry};
use serde_json::{Map, Value};

use super::output::{
    format_task_history_event, format_task_show_timestamp, is_human_visible_history_event,
    parse_timestamp, warn_unresolved_crew,
};
use super::support::attach_bound_workspace_identity;
use crate::command::{Block, CommandOut, Payload};
use crate::output::color::{Domain, bold, dimmed, text};

pub(super) fn render(
    runtime: &OrbitRuntime,
    input: &Map<String, Value>,
    value: Value,
) -> CommandOut {
    match input.get("fields").map(strings) {
        Some(fields) => {
            let text = fields_text(&value, &fields)?;
            Ok(Payload::detail(value, text).into())
        }
        None => record(runtime, input, value),
    }
}

/// The full record. The task may have been reached through the global
/// registry rather than the cwd, so it names where it was read from.
fn record(runtime: &OrbitRuntime, input: &Map<String, Value>, value: Value) -> CommandOut {
    let owner = bound_workspace_identity(runtime);
    let mut doc = attach_bound_workspace_identity(
        "orbit.task.show",
        &Value::Object(input.clone()),
        owner.as_ref(),
        value,
    )?;
    let object = doc.as_object_mut().ok_or_else(|| {
        OrbitError::Execution("task JSON projection did not produce an object".to_string())
    })?;
    // The guidance keys follow the workspace identity in the document.
    for key in ["next_step", "final_recovery"] {
        if let Some(guidance) = object.shift_remove(key) {
            object.insert(key.to_string(), guidance);
        }
    }
    warn_unresolved_crew(object);
    let task = &*object;

    let field = |key: &str| task.get(key).and_then(Value::as_str);
    let list = |key: &str| task.get(key).map(strings).unwrap_or_default();
    let mut out = String::new();
    let _ = writeln!(out, "{} {}", bold("ID:"), field("id").unwrap_or_default());
    if let Some(owner) = &owner {
        let _ = writeln!(out, "{} {}", bold("Workspace:"), owner.label());
    }
    if let Some(parent_id) = field("parent_id") {
        let _ = writeln!(out, "{} {}", bold("Parent Task:"), parent_id);
    }
    let _ = writeln!(
        out,
        "{} {}",
        bold("Title:"),
        field("title").unwrap_or_default()
    );
    let _ = writeln!(
        out,
        "{} {}",
        bold("Status:"),
        text(field("status").unwrap_or_default(), Domain::TaskStatus)
    );
    let _ = writeln!(
        out,
        "{} {}",
        bold("Priority:"),
        text(field("priority").unwrap_or_default(), Domain::Priority)
    );
    if let Some(complexity) = field("complexity") {
        let _ = writeln!(out, "{} {}", bold("Complexity:"), complexity);
    }
    if let Some(readiness) = task.get("readiness").and_then(readiness_text) {
        let _ = writeln!(out, "{} {readiness}", bold("Readiness:"));
    }
    let _ = writeln!(
        out,
        "{} {}",
        bold("Type:"),
        field("type").unwrap_or_default()
    );
    if let Some(description) = field("description").filter(|value| !value.is_empty()) {
        let _ = writeln!(out, "{} {}", bold("Description:"), description);
    }
    let criteria = list("acceptance_criteria");
    if !criteria.is_empty() {
        let _ = writeln!(out, "{}", bold("Acceptance Criteria:"));
        for criterion in criteria {
            let _ = writeln!(out, "  - {}", criterion);
        }
    }
    if !list("dependencies").is_empty() {
        let _ = writeln!(out, "{}", bold("Dependencies:"));
        for dependency in list("resolved_dependencies") {
            let _ = writeln!(out, "  - {}", dependency);
        }
    }
    let relations = task
        .get("relations")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    if !relations.is_empty() {
        let targets = relations
            .iter()
            .filter_map(|relation| relation.get("target").and_then(Value::as_str))
            .map(ToOwned::to_owned)
            .collect::<BTreeSet<_>>();
        let status_by_id = runtime.task_status_index_for(&targets)?;
        let _ = writeln!(out, "{}", bold("Relations:"));
        for relation in relations {
            let target = relation
                .get("target")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let status = relation
                .get("verification")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| status_by_id.get(target).map(ToString::to_string))
                .unwrap_or_else(|| "missing".to_string());
            let regression = relation
                .get("regression")
                .map(|regression| {
                    let part = |key| regression.get(key).and_then(Value::as_str);
                    format!(
                        " (nature: {}, escape: {})",
                        part("nature").unwrap_or_default(),
                        part("escape").unwrap_or_default()
                    )
                })
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "  - {}: {} [{}]{}",
                relation
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                target,
                status,
                regression
            );
        }
    }
    let artifacts = task
        .get("artifacts")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    if !artifacts.is_empty() {
        let _ = writeln!(out, "{}", bold("Artifacts:"));
        for artifact in artifacts {
            let _ = writeln!(out, "  - {}", artifact_label(artifact));
        }
    }
    let tags = list("tags");
    if !tags.is_empty() {
        let _ = writeln!(out, "{} {}", bold("Tags:"), tags.join(", "));
    }
    if let Some(os) = task.get("os_requirement").filter(|value| value.is_object()) {
        let invalid = os.get("invalid").map(strings).unwrap_or_default();
        let runs_on = if invalid.is_empty()
            || matches!(
                os.get("state").and_then(Value::as_str),
                Some("unresolved" | "conflict")
            ) {
            os.get("describe")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        } else {
            format!("no host until retagged ({})", invalid.join(", "))
        };
        let _ = writeln!(out, "{} {runs_on}", bold("Runs on:"));
    }
    let required_tools = list("required_tools");
    if !required_tools.is_empty() {
        let _ = writeln!(
            out,
            "{} {}",
            bold("Required Tools:"),
            required_tools.join(", ")
        );
    }
    let external_refs = task
        .get("external_refs")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    if !external_refs.is_empty() {
        let _ = writeln!(out, "{}", bold("External refs:"));
        for external_ref in external_refs {
            let part = |key: &str| {
                external_ref
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            };
            match external_ref.get("url").and_then(Value::as_str) {
                Some(url) => {
                    let _ = writeln!(out, "  - {}: {} [{}]", part("system"), part("id"), url);
                }
                None => {
                    let _ = writeln!(out, "  - {}: {}", part("system"), part("id"));
                }
            }
        }
    }
    if let Some(plan) = field("plan").filter(|value| !value.is_empty()) {
        let _ = writeln!(out, "{} {}", bold("Plan:"), plan);
    }
    if let Some(summary) = field("execution_summary").filter(|value| !value.is_empty()) {
        let _ = writeln!(out, "{} {}", bold("Execution Summary:"), summary);
    }
    let comments = sidecar::<TaskComment>(task.get("comments"))?;
    if !comments.is_empty() {
        let _ = writeln!(out, "{}", bold("Comments:"));
        for comment in &comments {
            let _ = writeln!(out, "  {}", comment_line(comment));
        }
    }
    let context_files = list("context_files");
    if !context_files.is_empty() {
        let _ = writeln!(out, "{} {}", bold("Context:"), context_files.join(", "));
    }
    for (label, key) in [
        ("Created By:", "created_by"),
        ("Planned By:", "planned_by"),
        ("Implemented By:", "implemented_by"),
        ("Execution Crew:", "crew"),
        ("Orchestrator:", "orchestrator"),
        ("Origin:", "origin"),
    ] {
        if let Some(value) = field(key) {
            let _ = writeln!(out, "{} {}", bold(label), value);
        }
    }
    let history = sidecar::<TaskHistoryEntry>(task.get("history"))?;
    let visible_history: Vec<_> = history
        .iter()
        .filter(|entry| is_human_visible_history_event(&entry.event))
        .collect();
    if !visible_history.is_empty() {
        let _ = writeln!(out, "{}", bold("History:"));
        for entry in visible_history {
            let _ = writeln!(out, "  {}", history_line(entry));
        }
    }
    // A run failure leaves the task `blocked`, which automation skips: say
    // what an operator does about it.
    if let Some(next) = task.get("next_step") {
        let _ = writeln!(out, "{} {}", bold("Next:"), next_step_text(next));
    }
    // The last automated final-recovery decision.
    if let Some(record) = task.get("final_recovery") {
        let _ = writeln!(
            out,
            "{} {}",
            bold("Final Recovery:"),
            final_recovery_text(record)
        );
    }
    // Criteria the delivery owes until the deployed build is measured.
    if let Some(owed) = task.get("owed_criteria") {
        let _ = writeln!(out, "{}", bold("Owed After Deploy:"));
        for line in owed_criteria_lines(owed) {
            let _ = writeln!(out, "  {line}");
        }
    }
    if let Some(pr_status) = field("pr_status") {
        let _ = writeln!(out, "{} {}", bold("PR Status:"), pr_status);
    }
    if let Some(review) = task.get("review") {
        let _ = writeln!(out, "{} {}", bold("Review:"), review);
    }
    if let Some(source_task_id) = field("source_task_id") {
        let _ = writeln!(out, "{} {}", bold("Source Task:"), source_task_id);
    }
    let _ = writeln!(
        out,
        "{} {}",
        bold("Created:"),
        dimmed(&timestamp(field("created_at").unwrap_or_default()))
    );
    let _ = writeln!(
        out,
        "{} {}",
        bold("Updated:"),
        dimmed(&timestamp(field("updated_at").unwrap_or_default()))
    );
    Ok(Payload::blocks(doc, vec![Block::text(out)]).into())
}

/// The text of a `--fields` projection: one field's value bare, several each
/// under a `Field:` heading.
fn fields_text(value: &Value, fields: &[&str]) -> Result<String, OrbitError> {
    let mut text = String::new();
    if let [field] = fields {
        // `terminal` alone stays keyed as an object.
        let value = if *field == "terminal" {
            value.get("terminal").unwrap_or(&Value::Null)
        } else {
            value
        };
        write_field(value, field, &mut text)?;
        return Ok(text);
    }
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            text.push('\n');
        }
        let _ = writeln!(text, "{} {}", bold("Field:"), field);
        write_field(value.get(*field).unwrap_or(&Value::Null), field, &mut text)?;
    }
    Ok(text)
}

fn write_field(value: &Value, field: &str, text: &mut String) -> Result<(), OrbitError> {
    match field {
        "comments" => {
            for comment in sidecar::<TaskComment>(Some(value))? {
                let _ = writeln!(text, "{}", comment_line(&comment));
            }
        }
        "history" => {
            for entry in sidecar::<TaskHistoryEntry>(Some(value))? {
                if is_human_visible_history_event(&entry.event) {
                    let _ = writeln!(text, "{}", history_line(&entry));
                }
            }
        }
        "acceptance_criteria" => {
            for criterion in strings(value) {
                let _ = writeln!(text, "- {}", criterion);
            }
        }
        "dependencies" | "resolved_dependencies" | "tags" | "required_tools" | "context_files" => {
            for item in strings(value) {
                let _ = writeln!(text, "{}", item);
            }
        }
        "relations" => {
            for relation in value.as_array().map_or(&[][..], Vec::as_slice) {
                let _ = writeln!(text, "{}", relation);
            }
        }
        "readiness" => {
            if let Some(readiness) = readiness_text(value) {
                text.push_str(&readiness);
            }
        }
        "artifacts" => {
            for (index, artifact) in value
                .as_array()
                .map_or(&[][..], Vec::as_slice)
                .iter()
                .enumerate()
            {
                if index > 0 {
                    text.push('\n');
                }
                let _ = writeln!(text, "{} {}", bold("Artifact:"), artifact_label(artifact));
            }
        }
        _ => match value {
            Value::String(value) => text.push_str(value),
            Value::Null => {}
            value => {
                let _ = write!(text, "{value}");
            }
        },
    }
    Ok(())
}

/// The human readiness readout of a `proposed` or `backlog` task: `ready` or
/// `not ready`, then one indented line per gap with its fix. `None` for any
/// other status, whose record carries no readiness.
fn readiness_text(readiness: &Value) -> Option<String> {
    let ready = readiness.get("ready")?.as_bool()?;
    let mut text = String::from(if ready { "ready" } else { "not ready" });
    for gap in readiness
        .get("gaps")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice)
    {
        let part = |key: &str| gap.get(key).and_then(Value::as_str).unwrap_or_default();
        let _ = write!(
            text,
            "\n  - {} [{}]: {} Fix: {}.",
            part("code"),
            part("severity"),
            part("message"),
            part("fix")
        );
    }
    Some(text)
}

/// An artifact's path, media type and size. The full record names the size
/// `size_bytes`; the `artifacts` projection names it `size`.
fn artifact_label(artifact: &Value) -> String {
    let part = |key: &str| {
        artifact
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    let size = artifact
        .get("size_bytes")
        .or_else(|| artifact.get("size"))
        .and_then(Value::as_u64)
        .unwrap_or_default();
    format!("{} ({}, {size} bytes)", part("path"), part("media_type"))
}

fn comment_line(comment: &TaskComment) -> String {
    format!(
        "{} {}: {}",
        dimmed(&format!("[{}]", format_task_show_timestamp(comment.at))),
        comment.by,
        super::pilot_comment::comment_presentation(comment)
    )
}

fn history_line(entry: &TaskHistoryEntry) -> String {
    let stamp = dimmed(&format!("[{}]", format_task_show_timestamp(entry.at)));
    match &entry.note {
        Some(note) => format!(
            "{stamp} {}: {} ({note})",
            entry.by,
            format_task_history_event(entry)
        ),
        None => format!("{stamp} {}: {}", entry.by, format_task_history_event(entry)),
    }
}

/// The words after `Next:` on a task show. Local and routed text both use this.
pub(crate) fn next_step_text(next: &Value) -> String {
    let part = |key: &str| next.get(key).and_then(Value::as_str).unwrap_or_default();
    match next.get("resume_command").and_then(Value::as_str) {
        Some(resume) => format!(
            "run `{resume}` to retry the failed work, or move the task back to backlog with `{}`",
            part("requeue_command")
        ),
        None => format!(
            "run {} cannot be resumed; move the task back to backlog with `{}` to run it again",
            part("run_id"),
            part("requeue_command")
        ),
    }
}

/// The words after `Final Recovery:` on a task show. Local and routed text
/// both use this.
pub(crate) fn final_recovery_text(record: &Value) -> String {
    let part = |key: &str| record.get(key).and_then(Value::as_str).unwrap_or_default();
    format!(
        "{} ({}) by run {} at {}",
        part("decision"),
        part("outcome"),
        part("run_id"),
        timestamp(part("at"))
    )
}

/// One line per owed criterion on a task show: its state, host, command and
/// target. Local and routed text both use this.
pub(crate) fn owed_criteria_lines(owed: &Value) -> Vec<String> {
    owed.as_array()
        .into_iter()
        .flatten()
        .map(|entry| {
            let part = |key: &str| entry.get(key).and_then(Value::as_str).unwrap_or_default();
            format!(
                "{}. [{}] on {}: `{}` (target {})",
                entry
                    .get("criterion")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
                part("state"),
                part("host"),
                part("command"),
                part("target")
            )
        })
        .collect()
}

/// A sidecar the tool serialized from its typed records.
fn sidecar<T: serde::de::DeserializeOwned>(value: Option<&Value>) -> Result<Vec<T>, OrbitError> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|error| OrbitError::Execution(format!("task show sidecar: {error}"))),
    }
}

fn timestamp(value: &str) -> String {
    parse_timestamp(value).map_or_else(|| value.to_string(), format_task_show_timestamp)
}

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}
