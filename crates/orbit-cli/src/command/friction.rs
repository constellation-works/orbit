//! The friction CLI, derived from the friction operation registry.
//!
//! ADR-0209 bearing 1 pilot [ORB-10358]. Each friction verb used to be a clap
//! `Args` struct plus an `Execute` impl that hand-built the tool input — the
//! same names, help strings, and field mapping the MCP tool file and the
//! dashboard handler each restated. All of that is now declared once in
//! `orbit_common::governance::friction::operations`, and the clap and input derivation is the
//! noun-agnostic adapter in [`super::operation_args`].
//!
//! Adding a friction verb requires no edit here. What is left in this file is
//! genuinely friction-specific: how to render a friction response as text.

use clap::{ArgMatches, Args, Command, FromArgMatches, Subcommand};
use orbit_common::governance::friction::{
    FRICTION_LIST_RESPONSE_MODE_WITH_NOTES, FRICTION_OPERATIONS, FrictionVerb, friction_operation,
};
use orbit_common::governance::operation::CliRender;
use orbit_core::OrbitRuntime;
use serde_json::Value;

use super::operation_args::{Invocation, augment_subcommands, invocation_from_matches};
use crate::command::{CommandOut, Execute, Payload};
use crate::output::payload::Block;

/// One parsed friction verb invocation.
pub type FrictionInvocation = Invocation<FrictionVerb>;

#[derive(Args)]
#[command(about = "Report, list, and triage friction records")]
pub struct FrictionCommand {
    #[command(subcommand)]
    pub command: FrictionInvocation,
}

impl Subcommand for FrictionInvocation {
    fn augment_subcommands(cmd: Command) -> Command {
        augment_subcommands(cmd, FRICTION_OPERATIONS)
    }

    fn augment_subcommands_for_update(cmd: Command) -> Command {
        <Self as Subcommand>::augment_subcommands(cmd)
    }

    fn has_subcommand(name: &str) -> bool {
        friction_operation(name).is_some()
    }
}

impl FromArgMatches for FrictionInvocation {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        invocation_from_matches(FRICTION_OPERATIONS, "friction", matches)
    }

    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl Execute for FrictionCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let FrictionInvocation {
            spec,
            mut input,
            json,
        } = self.command;
        // `--json` no longer picks a branch here: it resolves the sink's mode
        // in `main`, and the renderer projects whichever payload this builds.
        // The flag stays declared and accepted [ADR-0306].
        let _ = json;
        if spec.verb == FrictionVerb::List
            && let Some(object) = input.as_object_mut()
        {
            object.insert(
                "response_mode".to_string(),
                Value::String(FRICTION_LIST_RESPONSE_MODE_WITH_NOTES.to_string()),
            );
        }
        let value = runtime.run_tool(spec.tool_name, input)?;
        render(&value, spec.cli_render)
    }
}

/// Build a friction response's payload, per the spec's declared rendering.
fn render(value: &Value, kind: CliRender) -> CommandOut {
    match kind {
        CliRender::Record => record_payload(value),
        CliRender::RecordTable => records_table_payload(value),
        CliRender::TagList => tags_payload(value),
        // `AlwaysJson` responses have no useful flat rendering.
        _ => Ok(Payload::document(value.clone()).into()),
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
