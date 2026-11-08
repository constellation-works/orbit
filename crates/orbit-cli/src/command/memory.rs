//! The crew memory CLI, derived from the memory operation registry.
//!
//! Every verb calls its `orbit.memory.*` tool, so `--json` is the tool's
//! payload. What is left here is turning `write`'s repeatable `--file PATH`
//! into the tool's `files` input, and the text rendering.

use std::fmt::Write as _;
use std::path::Path;

use clap::Args;
use orbit_common::governance::memory::MemoryVerb;
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::{Map, Value, json};

use super::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};
use crate::output::payload::Block;
use crate::output::table::{Column, Table};

#[derive(Args)]
#[command(about = orbit_common::governance::memory::MEMORY.cli_about)]
pub struct MemoryCommand {
    #[command(subcommand)]
    pub command: Invocation<MemoryVerb>,
}

impl Execute for MemoryCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, mut input } = self.command;
        if spec.verb == MemoryVerb::Write {
            write_cli_input(&mut input)?;
        }
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        let blocks = match spec.verb {
            MemoryVerb::Show if value.get("spaces").is_some() => {
                vec![Block::table(spaces_table(&value))]
            }
            MemoryVerb::Show => vec![Block::text(space_text(&value))],
            MemoryVerb::History => history_blocks(&value),
            MemoryVerb::Export => vec![Block::text(export_text(&value))],
            // Tool-only: no CLI path reaches it.
            MemoryVerb::Propose => vec![Block::text(format!(
                "proposal `{}` opened",
                text(&value["id"])
            ))],
            MemoryVerb::Write | MemoryVerb::Reset | MemoryVerb::Copy => {
                vec![Block::text(format!(
                    "{} `{}` revision {} ({} file(s), {} bytes, {})",
                    text(&value["action"]).replace('_', " "),
                    text(&value["key"]),
                    text(&value["revision"]),
                    text(&value["file_count"]),
                    text(&value["bytes"]),
                    text(&value["digest"]),
                ))]
            }
        };
        Ok(Payload::blocks(value, blocks).into())
    }
}

/// Read each `--file PATH` into the tool's `files` input, named by its file
/// name. The tool's write rules check the names and content.
fn write_cli_input(input: &mut Map<String, Value>) -> Result<(), OrbitError> {
    let Some(paths) = input.remove("file_paths") else {
        return Ok(());
    };
    let mut files = Vec::new();
    for path in paths
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let text = std::fs::read_to_string(path).map_err(|error| {
            OrbitError::InvalidInput(format!("cannot read --file {path}: {error}"))
        })?;
        let name = Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        files.push(json!({ "path": name, "text": text }));
    }
    input.insert("files".into(), Value::Array(files));
    Ok(())
}

fn text(value: &Value) -> String {
    match value {
        Value::Null => "-".to_string(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn list(value: &Value) -> String {
    let items: Vec<String> = value.as_array().into_iter().flatten().map(text).collect();
    if items.is_empty() {
        "-".to_string()
    } else {
        items.join(", ")
    }
}

fn spaces_table(value: &Value) -> Table {
    let mut table = Table::new(vec![
        Column::new("KEY"),
        Column::new("REVISION"),
        Column::new("FILES"),
        Column::new("BYTES"),
        Column::new("WRITTEN"),
    ])
    .empty_message("no crew memory yet");
    for space in value["spaces"].as_array().into_iter().flatten() {
        table.add_row(
            ["key", "revision", "file_count", "bytes", "created_at"]
                .iter()
                .map(|key| text(&space[*key]))
                .collect(),
        );
    }
    table
}

fn space_text(value: &Value) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} revision {} · {}/{} file(s) · {}/{} bytes",
        text(&value["key"]),
        text(&value["revision"]),
        text(&value["file_count"]),
        text(&value["caps"]["max_files"]),
        text(&value["bytes"]),
        text(&value["caps"]["max_bytes"]),
    );
    if value["revision"].is_null() {
        let _ = write!(out, "No revision yet: the space is empty.");
        return out;
    }
    let _ = writeln!(out, "digest:   {}", text(&value["digest"]));
    let _ = writeln!(out, "evidence: {}", list(&value["evidence"]));
    let _ = writeln!(
        out,
        "writer:   {} {}",
        text(&value["writer"]["kind"]),
        text(&value["writer"]["actor"])
    );
    let _ = writeln!(out, "reason:   {}", text(&value["reason"]));
    let _ = write!(out, "written:  {}", text(&value["created_at"]));
    for file in value["files"].as_array().into_iter().flatten() {
        let _ = write!(
            out,
            "\n\n── {} ──\n{}",
            text(&file["path"]),
            text(&file["text"]).trim_end()
        );
    }
    out
}

fn history_blocks(value: &Value) -> Vec<Block> {
    let mut revisions = Table::new(vec![
        Column::new("REVISION"),
        Column::new("FILES"),
        Column::new("BYTES"),
        Column::new("WRITER"),
        Column::new("EVIDENCE"),
        Column::new("REASON"),
        Column::new("WRITTEN"),
    ])
    .empty_message("no revisions");
    for row in value["revisions"].as_array().into_iter().flatten() {
        revisions.add_row(vec![
            text(&row["revision"]),
            list(&row["files"]),
            text(&row["bytes"]),
            format!(
                "{} {}",
                text(&row["writer"]["kind"]),
                text(&row["writer"]["actor"])
            ),
            list(&row["evidence"]),
            text(&row["reason"]),
            text(&row["created_at"]),
        ]);
    }
    let mut events = Table::new(vec![
        Column::new("AT"),
        Column::new("ACTION"),
        Column::new("ACTOR"),
        Column::new("REVISION"),
        Column::new("DETAIL"),
    ])
    .empty_message("no events");
    for event in value["events"].as_array().into_iter().flatten() {
        let detail = &event["detail"];
        let summary = if matches!(
            event["action"].as_str(),
            Some("write_refused" | "propose_refused")
        ) {
            format!("{}: {}", text(&detail["rule"]), text(&detail["message"]))
        } else if detail["reason"].is_null() && !detail["task_id"].is_null() {
            text(&detail["task_id"])
        } else {
            text(&detail["reason"])
        };
        events.add_row(vec![
            text(&event["at"]),
            text(&event["action"]),
            text(&event["actor"]),
            text(&event["revision"]),
            summary,
        ]);
    }
    vec![
        Block::text(format!("{} revisions", text(&value["key"]))),
        Block::table(revisions),
        Block::text("events".to_string()),
        Block::table(events),
    ]
}

fn export_text(value: &Value) -> String {
    let files: Vec<String> = value["files"]
        .as_array()
        .into_iter()
        .flatten()
        .map(text)
        .collect();
    let mut out = format!(
        "Exported {} file(s) from {} space(s) to {}. Orbit never reads these files back.",
        files.len(),
        text(&value["spaces"]),
        text(&value["dir"])
    );
    for file in files {
        let _ = write!(out, "\n  {file}");
    }
    out
}
