//! The root attention CLI derives its inputs from the shared tool operation.
use super::{
    CommandOut, Execute, Payload,
    operation_args::{Invocation, augment_operation_args, input_from_matches},
};
use crate::output::{
    payload::Block,
    table::{Column, Table},
};
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
/// A federated envelope (one with `hosts`) adds a HOST column.
pub(crate) fn render_attention_envelope(value: Value) -> CommandOut {
    let federated = value["hosts"]
        .as_array()
        .is_some_and(|hosts| !hosts.is_empty());
    let mut columns = vec![Column::new("SEVERITY"), Column::new("KIND")];
    if federated {
        columns.push(Column::new("HOST"));
    }
    columns.extend([
        Column::new("WORKSPACE"),
        Column::new("REF"),
        Column::new("AGE"),
        Column::new("REASON"),
    ]);
    let mut table = Table::new(columns);
    let mut blocks = vec![Block::text(value["message"].as_str().unwrap_or_default())];
    let mut records = value["items"].as_array().cloned().unwrap_or_default();
    for item in &records {
        let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
        let mut row = vec![text(&item["severity"]), text(&item["kind"])];
        if federated {
            row.push(text(&item["host"]["name"]));
        }
        row.extend([
            text(&item["workspace"]["name"]),
            {
                let task = text(&item["refs"]["task_id"]);
                if task.is_empty() {
                    text(&item["refs"]["notice_id"])
                } else {
                    task
                }
            },
            item["age_seconds"]
                .as_u64()
                .map(|age| format!("{}h", age / 3600))
                .unwrap_or_default(),
            format!(
                "{}\n  {}",
                text(&item["reason"]),
                text(&item["next"]["command"])
            ),
        ]);
        table.add_row(row);
    }
    if !records.is_empty() {
        blocks.push(Block::table(table));
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
