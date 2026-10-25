//! The decision-request CLI, derived from the decision operation registry.
//!
//! `list` and `show` call the read-only `orbit.decision.*` tools. `answer`
//! is the operator's and never goes through the tool surface: it applies
//! the answer in core as this CLI's human writer. That is why an agent
//! session can list a request it is waiting on but cannot answer it.

use std::fmt::Write as _;

use clap::Args;
use orbit_common::governance::decision::DecisionVerb;
use orbit_core::{OrbitError, OrbitRuntime};
use serde_json::Value;

use super::operation_args::Invocation;
use crate::command::{CommandOut, Execute, Payload};
use crate::output::payload::Block;
use crate::output::table::{Column, Table};

#[derive(Args)]
#[command(about = orbit_common::governance::decision::DECISION.cli_about)]
pub struct DecisionCommand {
    #[command(subcommand)]
    pub command: Invocation<DecisionVerb>,
}

impl Execute for DecisionCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, input } = self.command;
        match spec.verb {
            DecisionVerb::List | DecisionVerb::Show => {
                let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
                let blocks = match spec.verb {
                    DecisionVerb::List => list_blocks(&value),
                    _ => show_blocks(&value),
                };
                Ok(Payload::blocks(value, blocks).into())
            }
            DecisionVerb::Answer => {
                let id = required(&input, "id")?;
                let answer = required(&input, "answer")?;
                let note = input
                    .get("note")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let label = runtime.actor_label().trim();
                let actor = if label.is_empty() {
                    "human".to_string()
                } else {
                    label.to_string()
                };
                let request = runtime.answer_decision_request_as_human(
                    &id,
                    &answer,
                    note,
                    actor,
                    Some("cli".to_string()),
                )?;
                let value = serde_json::to_value(&request).map_err(|error| {
                    OrbitError::Execution(format!("serialize decision request: {error}"))
                })?;
                let blocks = vec![Block::text(format!(
                    "Answered {} with `{answer}`; the request on {} now reads {}.",
                    request.id,
                    request.task_id,
                    request.state.as_str(),
                ))];
                Ok(Payload::blocks(value, blocks).into())
            }
        }
    }
}

fn required(input: &serde_json::Map<String, Value>, name: &str) -> Result<String, OrbitError> {
    input
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| OrbitError::InvalidInput(format!("`{name}` is required")))
}

fn list_blocks(value: &Value) -> Vec<Block> {
    let mut table = Table::new(vec![
        Column::new("REQUEST").fixed(),
        Column::new("TASK").fixed(),
        Column::new("GATE").fixed(),
        Column::new("STATE").fixed(),
        Column::new("ANSWERS"),
        Column::new("EXPIRES").fixed(),
    ])
    .empty_message("no decision requests");
    for request in value["requests"].as_array().into_iter().flatten() {
        let answers = request["answers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|answer| answer["id"].as_str())
            .collect::<Vec<_>>()
            .join(", ");
        table.add_row(vec![
            text(&request["id"]),
            text(&request["task_id"]),
            text(&request["gate"]),
            text(&request["state"]),
            answers,
            text(&request["expires_at"]),
        ]);
    }
    vec![Block::table(table)]
}

fn show_blocks(value: &Value) -> Vec<Block> {
    let mut out = format!(
        "{} ({}, {})\ntask {} · raised by {} at {} · expires {}",
        text(&value["id"]),
        text(&value["gate"]),
        text(&value["state"]),
        text(&value["task_id"]),
        text(&value["raised_by"]),
        text(&value["raised_at"]),
        text(&value["expires_at"]),
    );
    for (index, answer) in value["answers"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let _ = write!(
            out,
            "\n{}. {} (`{}`) — {}",
            index + 1,
            text(&answer["label"]),
            text(&answer["id"]),
            text(&answer["consequence"]),
        );
    }
    if let Some(binding) = value.get("binding") {
        let _ = write!(out, "\nbinding: {binding}");
    }
    if let Some(resolution) = value.get("resolution").filter(|value| !value.is_null()) {
        let _ = write!(
            out,
            "\nanswered: `{}` by {} at {}",
            text(&resolution["answer_id"]),
            text(&resolution["answered_by"]),
            text(&resolution["answered_at"]),
        );
        if let Some(note) = resolution["note"].as_str() {
            let _ = write!(out, "\nnote: {note}");
        }
    }
    vec![Block::text(out)]
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}
