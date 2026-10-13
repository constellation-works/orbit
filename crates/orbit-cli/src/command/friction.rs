//! The friction CLI, derived from the friction operation registry.
//!
//! ADR-0209 bearing 1 pilot [ORB-10358]. Each friction verb used to be a clap
//! `Args` struct plus an `Execute` impl that hand-built the tool input — the
//! same names, help strings, and field mapping the MCP tool file and the
//! dashboard handler each restated. All of that is now declared once in
//! `orbit_common::governance::friction::operations`, and the clap and input derivation is the
//! noun-agnostic adapter in [`super::operation_args`].
//!
//! What is left in this file is genuinely friction-specific and CLI-only: the
//! request shaping in [`prepare`] and the text rendering in [`render`]. Both
//! are exhaustive matches on [`FrictionVerb`], so a new verb fails to compile
//! here until its CLI behaviour is decided. Field presence rules live in the
//! spec; the adapter only words their refusal in flags.

use clap::Args;
use orbit_common::governance::friction::{FRICTION_LIST_RESPONSE_MODE_WITH_NOTES, FrictionVerb};
use orbit_core::OrbitRuntime;
use serde_json::{Map, Value};

use super::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};
use crate::output::payload::Block;

#[derive(Args)]
#[command(about = orbit_common::governance::friction::FRICTION.cli_about)]
pub struct FrictionCommand {
    #[command(subcommand)]
    pub command: Invocation<FrictionVerb>,
}

impl Execute for FrictionCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        self.command.require_at_least_one_of()?;
        let Invocation { spec, mut input } = self.command;
        prepare(spec.verb, &mut input)?;
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        render(spec.verb, &value)
    }
}

/// Shape a friction request for the CLI before it reaches the tool.
fn prepare(
    verb: FrictionVerb,
    input: &mut Map<String, Value>,
) -> Result<(), orbit_core::OrbitError> {
    match verb {
        // The table renderer shows the tool's guidance notes.
        FrictionVerb::List => {
            input.insert(
                "response_mode".to_string(),
                Value::String(FRICTION_LIST_RESPONSE_MODE_WITH_NOTES.to_string()),
            );
            Ok(())
        }
        FrictionVerb::Add
        | FrictionVerb::Show
        | FrictionVerb::Stats
        | FrictionVerb::Tags
        | FrictionVerb::Update
        | FrictionVerb::Resolve
        | FrictionVerb::Rehome => Ok(()),
    }
}

/// Build a friction response's text payload.
fn render(verb: FrictionVerb, value: &Value) -> CommandOut {
    match verb {
        FrictionVerb::Add
        | FrictionVerb::Show
        | FrictionVerb::Update
        | FrictionVerb::Resolve
        | FrictionVerb::Rehome => record_payload(value),
        FrictionVerb::List => records_table_payload(value),
        FrictionVerb::Tags => tags_payload(value),
        // Aggregate rates have no useful flat rendering.
        FrictionVerb::Stats => Ok(Payload::document(value.clone()).into()),
    }
}

fn records_table_payload(value: &Value) -> CommandOut {
    let Some((records, notes)) = split_list_payload(value) else {
        return Ok(Payload::document(value.clone()).into());
    };

    use crate::output::table::{Column, Table};
    // `orbit friction show <id>` prints a record in full.
    let mut table = Table::new(vec![
        Column::new("ID").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("MODEL").fixed(),
        Column::new("TAGS"),
        Column::new("TASK").fixed(),
        Column::new("TITLE"),
    ])
    .empty_message("no friction records matching the given filters");
    for record in &records {
        table.add_row(vec![
            value_string(record, "id"),
            value_string(record, "status"),
            value_string(record, "model"),
            value_string_list(record, "tags"),
            value_string(record, "during_task"),
            value_string(record, "title"),
        ]);
    }
    if notes.is_empty() {
        return Ok(Payload::list(records, table).into());
    }
    let mut blocks: Vec<Block> = notes
        .iter()
        .map(|note| Block::text(format!("note: {note}")))
        .collect();
    blocks.push(Block::table(table));
    Ok(Payload::blocks(Value::Array(records), blocks).into())
}

/// The tool's opt-in notes envelope is projected back to the historical record
/// array for JSON output while retaining notes in the human rendering.
fn split_list_payload(value: &Value) -> Option<(Vec<Value>, Vec<String>)> {
    match value {
        Value::Array(records) => Some((records.clone(), Vec::new())),
        Value::Object(object) => {
            let records = object.get("records").and_then(Value::as_array)?.clone();
            let notes = object
                .get("notes")
                .and_then(Value::as_array)
                .map(|notes| {
                    notes
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            Some((records, notes))
        }
        _ => None,
    }
}

fn record_payload(value: &Value) -> CommandOut {
    if !value.is_object() {
        return Ok(Payload::document(value.clone()).into());
    }

    let mut lines = vec![
        format!("ID: {}", value_string(value, "id")),
        format!("Status: {}", value_string(value, "status")),
        format!("Model: {}", value_string(value, "model")),
    ];
    let tags = value_string_list(value, "tags");
    if !tags.is_empty() {
        lines.push(format!("Tags: {tags}"));
    }
    let task = value_string(value, "during_task");
    if !task.is_empty() {
        lines.push(format!("Task: {task}"));
    }
    let rehome_to = value_string(value, "rehome_to");
    if !rehome_to.is_empty() {
        lines.push(format!("Re-home to: {rehome_to}"));
    }
    // `orbit friction rehome` answers with the resolved source record plus
    // the copy it created in the owning workspace.
    if let Some(moved) = value.get("rehomed_as") {
        lines.push(format!("Re-homed as: {}", value_string(moved, "id")));
    }
    let dropped = value_string_list(value, "dropped_tags");
    if !dropped.is_empty() {
        lines.push(format!("Dropped tags: {dropped}"));
    }
    // ADR-0345: `path` is the legacy evidence file an imported record came
    // from. Records written after the SQLite cutover carry `null` and render
    // no row rather than a location nothing could open.
    let path = value_string(value, "path");
    if !path.is_empty() {
        lines.push(format!("Legacy file: {path}"));
    }
    let body = value_string(value, "body");
    if !body.is_empty() {
        lines.push(format!("\n{body}"));
    }
    Ok(Payload::detail(value.clone(), lines.join("\n")).into())
}

fn tags_payload(value: &Value) -> CommandOut {
    let Some(tags) = value.as_array() else {
        return Ok(Payload::document(value.clone()).into());
    };
    let lines = tags
        .iter()
        .map(|tag| match tag {
            Value::String(name) => name.clone(),
            Value::Object(object) => {
                let name = object
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let description = object
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if description.is_empty() {
                    name.to_string()
                } else {
                    format!("{name}\t{description}")
                }
            }
            other => other.to_string(),
        })
        .collect::<Vec<_>>();
    Ok(Payload::detail(value.clone(), lines.join("\n")).into())
}

fn value_string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn value_string_list(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}
