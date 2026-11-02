//! The decision-request CLI, derived from the decision operation registry.
//!
//! `list` and `show` call the read-only `orbit.decision.*` tools. `answer`
//! is the operator's and never goes through the tool surface: it applies
//! the answer in core as this CLI's human writer. That is why an agent
//! session can list a request it is waiting on but cannot answer it.
//! `submit` goes through the governed `orbit.decision.submit` tool so the
//! operator tests a channel on exactly the path its plugin uses.
//! `digest` likewise goes through the governed `orbit.decision.digest`
//! tool, raising the approval requests the plugin's digest presents.
//! `channel …` administers this host's registrations in core directly;
//! registering, revoking and rotating are refused from an agent envelope
//! here, and the CLI's pre-dispatch gate already refuses every plain
//! command to a plugin callback, so no running code can mint itself a
//! human. `assist …` enables, disables and shows precedent-decision
//! categories in core's host-only store; enabling and disabling are
//! refused from an agent envelope here and in core, and plugin callbacks
//! are refused by the same pre-dispatch gate.

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
            DecisionVerb::List
            | DecisionVerb::Show
            | DecisionVerb::Raise
            | DecisionVerb::Submit
            | DecisionVerb::Digest => {
                let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
                let blocks = match spec.verb {
                    DecisionVerb::List => list_blocks(&value),
                    DecisionVerb::Digest => digest_blocks(&value),
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
            DecisionVerb::AssistEnable
            | DecisionVerb::AssistDisable
            | DecisionVerb::AssistStatus => assist_execute(runtime, spec.verb, &input),
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
            DecisionVerb::Reverse => {
                let id = required(&input, "id")?;
                let actor = operator_label(runtime);
                let request = runtime.reverse_decision_answer(&id, &actor)?;
                let value = serde_json::to_value(&request).map_err(|error| {
                    OrbitError::Execution(format!("serialize decision request: {error}"))
                })?;
                let blocks = vec![Block::text(format!(
                    "Re-opened {}; the request on {} now reads {}.",
                    request.id,
                    request.task_id,
                    request.state.as_str(),
                ))];
                Ok(Payload::blocks(value, blocks).into())
            }
            DecisionVerb::RuleAuto => {
                let rule = required(&input, "rule")?;
                let setting = required(&input, "auto")?;
                let auto = match setting.as_str() {
                    "on" => true,
                    "off" => false,
                    other => {
                        return Err(OrbitError::InvalidInput(format!(
                            "`auto` must be on or off, not `{other}`"
                        )));
                    }
                };
                let actor = operator_label(runtime);
                runtime.set_decision_rule_auto(&rule, auto, &actor)?;
                let value = serde_json::json!({ "rule": rule, "auto": auto });
                let word = if auto { "on" } else { "off" };
                let blocks = vec![Block::text(format!("Set auto {word} on `{rule}`."))];
                Ok(Payload::blocks(value, blocks).into())
            }
            DecisionVerb::Consolidate => {
                let since = input.get("since").and_then(Value::as_str);
                let value = runtime.consolidate_decision_precedent(since)?;
                let count = value.get("count").and_then(Value::as_u64).unwrap_or(0);
                let paths = value
                    .get("proposals")
                    .and_then(Value::as_array)
                    .map(|proposals| {
                        proposals
                            .iter()
                            .filter_map(|proposal| proposal.get("path").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                let blocks = vec![Block::text(format!(
                    "Proposed {count} corpus file(s). Nothing was written. Open a pull request with these proposals; do not edit a rule outside that pull request.\n{paths}"
                ))];
                Ok(Payload::blocks(value, blocks).into())
            }
        }
    }
}

fn operator_label(runtime: &OrbitRuntime) -> String {
    let label = runtime.actor_label().trim();
    if label.is_empty() {
        "human".to_string()
    } else {
        label.to_string()
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
            let require_dkim_verification = input
                .get("require_dkim_verification")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let info = runtime.register_decision_channel_with_dkim_requirement(
                &channel,
                &human,
                &plugin,
                allowed_senders,
                require_dkim_verification,
            )?;
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
                Column::new("DKIM").fixed(),
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
                    if channel.require_dkim_verification {
                        "required"
                    } else {
                        "optional"
                    }
                    .to_string(),
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

fn assist_execute(
    runtime: &OrbitRuntime,
    verb: DecisionVerb,
    input: &serde_json::Map<String, Value>,
) -> CommandOut {
    if !matches!(verb, DecisionVerb::AssistStatus)
        && orbit_common::governance::authorization::agent_context_declared()
    {
        return Err(OrbitError::PolicyDenied(
            "enabling or disabling a precedent-decision category is the operator's: it is refused from an agent envelope".into(),
        ));
    }
    fn serialize<T: serde::Serialize>(value: &T) -> Result<Value, OrbitError> {
        serde_json::to_value(value).map_err(|error| {
            OrbitError::Execution(format!("serialize precedent-decision enablements: {error}"))
        })
    }
    let operator = operator_label(runtime);
    match verb {
        DecisionVerb::AssistEnable => {
            let category = required(input, "category")?;
            let readout = required(input, "readout")?;
            let status = runtime.enable_precedent_assist(&category, &readout, &operator)?;
            let record = &status.enablement;
            let blocks = vec![Block::text(format!(
                "Enabled model answers in `{}` at threshold {:.2} under policy {} ({}), model revision {}, from readout {}. Only routine and reversible requests are answered; {:.0}% go to you anyway.",
                record.category,
                record.threshold,
                record.policy_version,
                record.policy_digest,
                record.model_revision,
                record.readout,
                record.holdback_share * 100.0,
            ))];
            Ok(Payload::blocks(serialize(&status)?, blocks).into())
        }
        DecisionVerb::AssistDisable => {
            let category = required(input, "category")?;
            let status = runtime.disable_precedent_assist(&category, &operator)?;
            let blocks = vec![Block::text(format!(
                "`{}` is {}; its open requests wait for a human answer.",
                status.enablement.category, status.effective_state
            ))];
            Ok(Payload::blocks(serialize(&status)?, blocks).into())
        }
        DecisionVerb::AssistStatus => {
            let statuses = runtime.precedent_assist_status()?;
            let mut table = Table::new(vec![
                Column::new("CATEGORY"),
                Column::new("STATE").fixed(),
                Column::new("THRESHOLD").fixed(),
                Column::new("POLICY").fixed(),
                Column::new("WINDOW").fixed(),
                Column::new("READOUT"),
                Column::new("ENDED"),
            ])
            .empty_message("no precedent-decision categories enabled on this host");
            for status in &statuses {
                let record = &status.enablement;
                let agreed = record
                    .window
                    .iter()
                    .filter(|outcome| outcome.agreed)
                    .count();
                let ended = record
                    .ended
                    .as_ref()
                    .map(|ended| format!("{} by {} at {}", ended.reason, ended.by, ended.at))
                    .unwrap_or_default();
                table.add_row(vec![
                    record.category.clone(),
                    status.effective_state.clone(),
                    format!("{:.2}", record.threshold),
                    record.policy_version.clone(),
                    format!("{agreed}/{}", record.window.len()),
                    record.readout.clone(),
                    ended,
                ]);
            }
            Ok(Payload::blocks(serialize(&statuses)?, vec![Block::table(table)]).into())
        }
        _ => Err(OrbitError::Execution(
            "decision assist dispatch reached a non-assist verb".into(),
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
        Column::new("RISK").fixed(),
        Column::new("CATEGORY"),
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
            text(&request["risk_tier"]),
            text(&request["category"]),
            answers,
            text(&request["expires_at"]),
        ]);
    }
    let mut blocks = vec![Block::table(table)];
    for warning in value["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        blocks.push(Block::text(format!("warning: {warning}")));
    }
    blocks
}

fn digest_blocks(value: &Value) -> Vec<Block> {
    let raised = value["raised"].as_array().map_or(0, Vec::len);
    let expiring = value["expiring"].as_array().map_or(0, Vec::len);
    let model_answered = value["model_answered"].as_array().map_or(0, Vec::len);
    let mut blocks = vec![Block::text(format!(
        "Digest: {raised} request(s) raised, {expiring} open request(s) expiring within 24 hours, {model_answered} model answer(s) awaiting review."
    ))];
    blocks.extend(list_blocks(value));
    if model_answered > 0 {
        let mut table = Table::new(vec![
            Column::new("REVIEW").fixed(),
            Column::new("REQUEST").fixed(),
            Column::new("TASK").fixed(),
            Column::new("ANSWER").fixed(),
            Column::new("PROBABILITY").fixed(),
            Column::new("PRECEDENTS"),
        ]);
        for entry in value["model_answered"].as_array().into_iter().flatten() {
            let resolution = &entry["request"]["resolution"];
            let model = &resolution["model"];
            let precedents = model["cited_precedents"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|precedent| precedent["precedent_id"].as_str())
                .collect::<Vec<_>>()
                .join(", ");
            table.add_row(vec![
                text(&entry["review"]["id"]),
                text(&entry["request"]["id"]),
                text(&entry["request"]["task_id"]),
                text(&resolution["answer_id"]),
                model["probability"]
                    .as_f64()
                    .map(|probability| format!("{probability:.2}"))
                    .unwrap_or_default(),
                precedents,
            ]);
        }
        blocks.push(Block::table(table));
    }
    blocks
}

fn show_blocks(value: &Value) -> Vec<Block> {
    let mut out = format!(
        "{} ({}, {})\ntask {} · raised by {} at {} · expires {}\nrisk: {} · category: {}",
        text(&value["id"]),
        text(&value["gate"]),
        text(&value["state"]),
        text(&value["task_id"]),
        text(&value["raised_by"]),
        text(&value["raised_at"]),
        text(&value["expires_at"]),
        text(&value["risk_tier"]),
        text(&value["category"]),
    );
    for (index, answer) in value["answers"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let _ = write!(
            out,
            "\n{}. {} (`{}`; {}) — {}",
            index + 1,
            text(&answer["label"]),
            text(&answer["id"]),
            text(&answer["risk_tier"]),
            text(&answer["consequence"]),
        );
    }
    if let Some(topic) = value.get("topic").filter(|value| !value.is_null()) {
        let _ = write!(out, "\ntopic: {}", text(topic));
    }
    if let Some(binding) = value.get("binding") {
        let _ = write!(out, "\nbinding: {binding}");
    }
    if let Some(grant) = value.get("script_grant").filter(|value| value.is_object()) {
        script_grant_section(&mut out, grant);
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
        if let Some(model) = resolution.get("model").filter(|value| !value.is_null()) {
            let _ = write!(
                out,
                "\nmodel: probability {} (threshold {}), {} cited precedent(s), category {}",
                model["probability"],
                model["threshold"],
                model["cited_precedents"].as_array().map_or(0, Vec::len),
                text(&model["category"]),
            );
            match resolution
                .get("confirmation")
                .filter(|value| !value.is_null())
            {
                Some(confirmation) => {
                    let _ = write!(
                        out,
                        "\nconfirmed by {}",
                        text(&confirmation["confirmed_by"])
                    );
                }
                None => out.push_str("\nunconfirmed model answer"),
            }
        }
    }
    vec![Block::text(out)]
}

/// What a `script_grant` answer consents to: the script, both digests, the
/// refusal, requested against granted capabilities and the diff to review.
fn script_grant_section(out: &mut String, grant: &Value) {
    let _ = write!(
        out,
        "\nscript: {} (entry {})\ndigest: {} (granted: {})\nrefusal: {} — {}\nrequested: {}\ngranted: {}",
        text(&grant["name"]),
        text(&grant["entry"]),
        text(&grant["digest"]),
        grant["granted_digest"].as_str().unwrap_or("none"),
        text(&grant["refusal"]),
        text(&grant["refusal_message"]),
        grant["requested"],
        if grant["granted"].is_null() {
            "none".to_string()
        } else {
            grant["granted"].to_string()
        },
    );
    let diff = text(&grant["diff"]);
    if !diff.is_empty() {
        let _ = write!(out, "\ndiff:\n{}", diff.trim_end());
    }
    if grant["diff_truncated"].as_bool() == Some(true) {
        out.push_str("\n(diff truncated)");
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}
