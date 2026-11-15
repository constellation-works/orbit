//! The root attention CLI derives its inputs from the shared tool operation.
use super::{
    CommandOut, Execute, Payload,
    operation_args::{Invocation, augment_operation_args, input_from_matches},
};
use crate::output::payload::{AttentionRow, Block};
use clap::{ArgMatches, Args, Command, FromArgMatches};
use orbit_common::governance::{attention::AttentionVerb, operation::OperationVerb};
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};

pub struct AttentionCommand {
    pub query: Invocation<AttentionVerb>,
}
impl Args for AttentionCommand {
    fn augment_args(cmd: Command) -> Command {
        augment_operation_args(cmd, AttentionVerb::Query.spec())
    }
    fn augment_args_for_update(cmd: Command) -> Command {
        Self::augment_args(cmd)
    }
}
impl FromArgMatches for AttentionCommand {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        let spec = AttentionVerb::Query.spec();
        Ok(Self {
            query: Invocation {
                spec,
                input: input_from_matches(spec, matches),
            },
        })
    }
    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}
impl Execute for AttentionCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let value = runtime.run_tool(self.query.spec.tool_name, Value::Object(self.query.input))?;
        render_attention_envelope(value)
    }
}

/// Render an attention envelope, local or merged by the federated client.
/// A federated envelope (one with `hosts`) includes the host in each item.
pub(crate) fn render_attention_envelope(value: Value) -> CommandOut {
    let federated = value["hosts"]
        .as_array()
        .is_some_and(|hosts| !hosts.is_empty());
    let mut blocks = vec![Block::text(value["message"].as_str().unwrap_or_default())];
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
        blocks.push(Block::Attention(rows));
    }
    for degraded in value["degraded"].as_array().into_iter().flatten() {
        blocks.push(Block::text(format!(
            "{} {}: {}",
            degraded["scope"].as_str().unwrap_or_default(),
            degraded["kind"].as_str().unwrap_or_default(),
            degraded["message"].as_str().unwrap_or_default()
        )));
    }
    records.push(json!({"summary": {"counts": value["counts"], "cursor": value["cursor"], "degraded": value["degraded"]}}));
    Ok(Payload::blocks(value, blocks).with_records(records).into())
}
