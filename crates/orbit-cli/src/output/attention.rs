//! Human and structured output for `orbit attention` and `orbit attention wait`:
//! a labelled block per item on a terminal, and one tab-separated record per
//! item in plain mode.

use orbit_common::OrbitError;
use orbit_core::app::attention::{AttentionWaitOutcome, AttentionWaitStatus};
use serde_json::{Value, json};

use crate::output::common::payload::{Block, CommandOutput, Payload};
use crate::output::common::sink::{OutputMode, OutputSink};
use crate::output::common::table::escape_plain;

/// Exit code when the wait ended with an empty delta and unchanged `degraded` set.
const WAIT_EXIT_TIMEOUT: i32 = 3;

/// Exit code when the delta is empty but the `degraded` set changed.
const WAIT_EXIT_DEGRADED_CHANGED: i32 = 4;

/// Exit code when the final read before the deadline failed, so the current state is unknown.
const WAIT_EXIT_UNREADABLE: i32 = 5;

/// The values used to render one operator attention item in human output.
#[derive(Debug)]
struct AttentionRow {
    severity: String,
    kind: String,
    host: Option<String>,
    workspace: String,
    reference: String,
    age: String,
    reason: String,
    next_command: String,
}

impl AttentionRow {
    /// The tab-separated plain-form header for `rows`: one label per field
    /// that [`Self::plain_fields`] emits, so the global `--header` labels
    /// attention the way it labels every other plain table.
    fn plain_header(rows: &[AttentionRow]) -> String {
        let mut labels = vec!["SEVERITY", "KIND"];
        if rows.iter().any(|row| row.host.is_some()) {
            labels.push("HOST");
        }
        labels.extend(["WORKSPACE", "REF", "AGE", "REASON"]);
        if rows.iter().any(|row| !row.next_command.is_empty()) {
            labels.push("NEXT");
        }
        labels.join("\t")
    }

    /// Format a labeled item block for terminal display.
    fn terminal_text(&self) -> String {
        let mut heading = format!("{} {}", self.severity, self.kind);
        if let Some(host) = self.host.as_ref().filter(|host| !host.is_empty()) {
            heading.push_str(&format!(" on {host}"));
        }
        if !self.workspace.is_empty() {
            heading.push_str(&format!(" — {}", self.workspace));
        }
        if !self.reference.is_empty() {
            heading.push_str(&format!(" ({})", self.reference));
        }
        if !self.age.is_empty() {
            heading.push_str(&format!(" [{}]", self.age));
        }

        let mut text = format!("{heading}\n  Reason: {}", self.reason);
        if !self.next_command.is_empty() {
            text.push_str(&format!("\n  Next: {}", self.next_command));
        }
        text
    }

    /// Fields for the stable plain record form, omitting an absent next action.
    fn plain_fields(&self) -> Vec<&str> {
        let mut fields = vec![
            self.severity.as_str(),
            self.kind.as_str(),
            self.workspace.as_str(),
            self.reference.as_str(),
            self.age.as_str(),
            self.reason.as_str(),
        ];
        if let Some(host) = self.host.as_deref() {
            fields.insert(2, host);
        }
        if !self.next_command.is_empty() {
            fields.push(self.next_command.as_str());
        }
        fields
    }
}

/// Render an attention envelope, local or merged by the federated client.
/// A federated envelope (one with `hosts`) includes the host in each item.
pub(crate) fn envelope_output(value: Value) -> Result<CommandOutput, OrbitError> {
    Ok(attention_payload(value, None).into())
}

/// Render a finished wait: the envelope plus a `wait` record naming why it
/// returned, with the exit code that tells a shell caller the same.
pub(crate) fn wait_output(outcome: AttentionWaitOutcome) -> Result<CommandOutput, OrbitError> {
    let (label, exit_code) = match outcome.status {
        AttentionWaitStatus::Changed => ("changed", 0),
        AttentionWaitStatus::DegradedChanged => ("degraded_changed", WAIT_EXIT_DEGRADED_CHANGED),
        AttentionWaitStatus::TimedOut => ("timeout", WAIT_EXIT_TIMEOUT),
        AttentionWaitStatus::Unreadable => ("unreadable", WAIT_EXIT_UNREADABLE),
    };
    let degraded_count = outcome
        .response
        .as_ref()
        .and_then(|response| response["degraded"].as_array())
        .map_or(0, Vec::len);
    let mut wait = json!({
        "status": label,
        "exit_code": exit_code,
        "waited_seconds": outcome.waited_seconds,
        // An empty `degraded` is the only all-clear a timeout can claim.
        "all_sources_readable": outcome.response.is_some() && degraded_count == 0,
    });
    if let Some(error) = &outcome.error {
        wait["error"] = json!(error);
    }
    let note = match outcome.status {
        AttentionWaitStatus::Changed => "attention changed".to_string(),
        AttentionWaitStatus::DegradedChanged => "the set of unreadable sources changed".to_string(),
        AttentionWaitStatus::TimedOut if degraded_count == 0 => {
            "timed out; nothing changed".to_string()
        }
        AttentionWaitStatus::TimedOut => format!(
            "timed out; nothing changed, but {degraded_count} source(s) were unreadable, so this is not all-clear"
        ),
        AttentionWaitStatus::Unreadable => format!(
            "attention could not be read: {}",
            outcome.error.as_deref().unwrap_or("unknown error")
        ),
    };
    let Some(mut response) = outcome.response else {
        let payload = Payload::detail(json!({"wait": wait}), note);
        return Ok(payload
            .with_exit_code(exit_code)
            .with_audit_message(label)
            .into());
    };
    response["wait"] = wait;
    Ok(attention_payload(response, Some(note))
        .with_exit_code(exit_code)
        .with_audit_message(label)
        .into())
}

fn attention_payload(value: Value, note: Option<String>) -> Payload {
    let federated = value["hosts"]
        .as_array()
        .is_some_and(|hosts| !hosts.is_empty());
    let mut blocks: Vec<Block> = note.map(Block::text).into_iter().collect();
    blocks.push(Block::text(value["message"].as_str().unwrap_or_default()));
    let mut records = value["items"].as_array().cloned().unwrap_or_default();
    let mut rows = Vec::with_capacity(records.len());
    for item in &records {
        let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
        let task = text(&item["refs"]["task_id"]);
        let reference = if task.is_empty() {
            text(&item["refs"]["notice_id"])
        } else {
            task
        };
        rows.push(AttentionRow {
            severity: text(&item["severity"]),
            kind: text(&item["kind"]),
            host: federated.then(|| text(&item["host"]["name"])),
            workspace: text(&item["workspace"]["name"]),
            reference,
            age: item["age_seconds"]
                .as_u64()
                .map(|age| format!("{}h", age / 3600))
                .unwrap_or_default(),
            reason: text(&item["reason"]),
            next_command: text(&item["next"]["command"]),
        });
    }
    if !rows.is_empty() {
        blocks.push(attention_rows_block(rows));
    }
    for host in value["acknowledged"].as_array().into_iter().flatten() {
        if let (Some(name), Some(reason)) = (host["name"].as_str(), host["reason"].as_str()) {
            blocks.push(Block::text(format!(
                "{name} acknowledged offline: {reason}"
            )));
        }
    }
    for degraded in value["degraded"].as_array().into_iter().flatten() {
        blocks.push(Block::text(format!(
            "{} {}: {}",
            degraded["scope"].as_str().unwrap_or_default(),
            degraded["kind"].as_str().unwrap_or_default(),
            degraded["message"].as_str().unwrap_or_default()
        )));
    }
    let mut summary = json!({
        "counts": value["counts"],
        "cursor": value["cursor"],
        "degraded": value["degraded"],
    });
    if let Some(acknowledged) = value.get("acknowledged") {
        summary["acknowledged"] = acknowledged.clone();
    }
    records.push(json!({"summary": summary}));
    Payload::blocks(value, blocks).with_records(records)
}

fn attention_rows_block(rows: Vec<AttentionRow>) -> Block {
    Block::custom(move |sink| emit_attention_rows(&rows, sink))
}

fn emit_attention_rows(rows: &[AttentionRow], sink: &OutputSink) {
    if sink.mode() == OutputMode::Table {
        for row in rows {
            println!("{}", sink.wrap_prose(&row.terminal_text()));
        }
    } else {
        if sink.plain_header() && !rows.is_empty() {
            println!("{}", AttentionRow::plain_header(rows));
        }
        for row in rows {
            let fields = row
                .plain_fields()
                .into_iter()
                .map(escape_plain)
                .collect::<Vec<_>>()
                .join("\t");
            println!("{fields}");
        }
    }
}
