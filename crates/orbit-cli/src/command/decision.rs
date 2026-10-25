//! The decision-request CLI, derived from the decision operation registry.
//!
//! `list` and `show` call the read-only `orbit.decision.*` tools. `answer`
//! is the operator's and never goes through the tool surface: it applies
//! the answer in core as this CLI's human writer. That is why an agent
//! session can list a request it is waiting on but cannot answer it.
//! `submit` goes through the governed `orbit.decision.submit` tool so the
//! operator tests a channel on exactly the path its plugin uses.
//! `channel …` administers this host's registrations in core directly;
//! registering, revoking and rotating are refused from an agent envelope
//! here, and the CLI's pre-dispatch gate already refuses every plain
//! command to a plugin callback, so no running code can mint itself a
//! human.

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
            DecisionVerb::List | DecisionVerb::Show | DecisionVerb::Submit => {
                let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
                let blocks = match spec.verb {
                    DecisionVerb::List => list_blocks(&value),
                    DecisionVerb::Submit => vec![Block::text(format!(
                        "Submitted `{}` for {}; the request on {} now reads {}.",
                        text(&value["resolution"]["answer_id"]),
                        text(&value["id"]),
                        text(&value["task_id"]),
                        text(&value["state"]),
                    ))],
                    _ => show_blocks(&value),
                };
                Ok(Payload::blocks(value, blocks).into())
            }
            DecisionVerb::ChannelRegister
            | DecisionVerb::ChannelList
            | DecisionVerb::ChannelRevoke
            | DecisionVerb::ChannelRotateKey => channel_execute(runtime, spec.verb, &input),
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

fn channel_execute(
    runtime: &OrbitRuntime,
    verb: DecisionVerb,
    input: &serde_json::Map<String, Value>,
) -> CommandOut {
    if !matches!(verb, DecisionVerb::ChannelList)
        && orbit_common::governance::authorization::agent_context_declared()
    {
        return Err(OrbitError::PolicyDenied(
            "registering, revoking or rotating a decision channel is the operator's: it is refused from an agent envelope, so no running code can mint itself a human".into(),
        ));
    }
    fn serialize<T: serde::Serialize>(value: &T) -> Result<Value, OrbitError> {
        serde_json::to_value(value)
            .map_err(|error| OrbitError::Execution(format!("serialize decision channels: {error}")))
    }
    match verb {
        DecisionVerb::ChannelRegister => {
            let channel = required(input, "channel")?;
            let human = required(input, "human")?;
            let plugin = required(input, "plugin")?;
            let allowed_senders = input
                .get("allowed_senders")
                .and_then(Value::as_array)
                .map(|senders| {
                    senders
                        .iter()
                        .filter_map(|sender| sender.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let info =
                runtime.register_decision_channel(&channel, &human, &plugin, allowed_senders)?;
            let blocks = vec![Block::text(format!(
                "Registered decision channel `{}` for {} via plugin {} (key epoch {}, fingerprint sha256:{}).",
                info.channel, info.human, info.plugin, info.epoch, info.key_fingerprint
            ))];
            Ok(Payload::blocks(serialize(&info)?, blocks).into())
        }
        DecisionVerb::ChannelList => {
            let channels = runtime.list_decision_channels()?;
            let value = serialize(&channels)?;
            let mut table = Table::new(vec![
                Column::new("CHANNEL").fixed(),
                Column::new("HUMAN").fixed(),
                Column::new("PLUGIN").fixed(),
                Column::new("EPOCH").fixed(),
                Column::new("STATE").fixed(),
                Column::new("SENDERS"),
            ])
            .empty_message("no decision channels registered on this host");
            for channel in &channels {
                table.add_row(vec![
                    channel.channel.clone(),
                    channel.human.clone(),
                    channel.plugin.clone(),
                    channel.epoch.to_string(),
                    if channel.revoked { "revoked" } else { "active" }.to_string(),
                    channel.allowed_senders.join(", "),
                ]);
            }
            Ok(Payload::blocks(value, vec![Block::table(table)]).into())
        }
        DecisionVerb::ChannelRevoke => {
            let channel = required(input, "channel")?;
            let info = runtime.revoke_decision_channel(&channel)?;
            let blocks = vec![Block::text(format!(
                "Revoked decision channel `{}`; its outstanding tokens no longer verify.",
                info.channel
            ))];
            Ok(Payload::blocks(serialize(&info)?, blocks).into())
        }
        DecisionVerb::ChannelRotateKey => {
            let channel = required(input, "channel")?;
            let info = runtime.rotate_decision_channel_key(&channel)?;
            let blocks = vec![Block::text(format!(
                "Rotated decision channel `{}` to key epoch {} (fingerprint sha256:{}); every outstanding token is invalidated.",
                info.channel, info.epoch, info.key_fingerprint
            ))];
            Ok(Payload::blocks(serialize(&info)?, blocks).into())
        }
        _ => Err(OrbitError::Execution(
            "decision channel dispatch reached a non-channel verb".into(),
        )),
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
