//! The `orbit operation` CLI, derived from the operation-mode registry
//! [ORB-11332].
//!
//! Every verb is declared once in
//! `orbit_common::governance::operation_mode`; clap wiring and the tool input
//! come from the noun-agnostic adapter in [`super::operation_args`]. What is
//! left here is presentation: how a grant or an explanation renders as text.

use clap::{ArgMatches, Args, Command, FromArgMatches, Subcommand};
use orbit_common::governance::operation::CliRender;
use orbit_common::governance::operation_mode::{
    OPERATION_MODE_OPERATIONS, OperationModeVerb, operation_mode_operation,
};
use orbit_core::OrbitRuntime;
use serde_json::Value;

use super::operation_args::{Invocation, augment_subcommands, invocation_from_matches};
use crate::command::{CommandOut, Execute, Payload};
use crate::output::table::{Column, Table};

/// One parsed operation-mode verb invocation.
pub type OperationModeInvocation = Invocation<OperationModeVerb>;

#[derive(Args)]
#[command(
    about = "Explain, enable, stop, and revoke scoped operation-mode automation",
    after_help = "Preferences live under `[operation]` in config.toml and authorize nothing by\n\
                  themselves. `orbit operation enable` records the one durable grant: a finite\n\
                  task set, separate prepare/promote/complete rights, and a bounded window.\n\
                  Start a drain under it with `orbit run auto --grant <ID>`. `stop` ends new\n\
                  admissions while admitted work keeps its captured bounds; `revoke` also\n\
                  withdraws completion from admitted work. Neither cancels running children."
)]
pub struct OperationModeCommand {
    #[command(subcommand)]
    pub command: OperationModeInvocation,
}

impl Subcommand for OperationModeInvocation {
    fn augment_subcommands(cmd: Command) -> Command {
        augment_subcommands(cmd, OPERATION_MODE_OPERATIONS)
    }

    fn augment_subcommands_for_update(cmd: Command) -> Command {
        <Self as Subcommand>::augment_subcommands(cmd)
    }

    fn has_subcommand(name: &str) -> bool {
        operation_mode_operation(name).is_some()
    }
}

impl FromArgMatches for OperationModeInvocation {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        invocation_from_matches(OPERATION_MODE_OPERATIONS, "operation", matches)
    }

    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl Execute for OperationModeCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let OperationModeInvocation { spec, input, json } = self.command;
        let _ = json;
        let value = runtime.run_tool(spec.tool_name, input)?;
        match spec.cli_render {
            CliRender::Record => grant_payload(&value),
            CliRender::RecordTable => grants_table_payload(&value),
            _ => Ok(Payload::document(value.clone()).into()),
        }
    }
}

fn grants_table_payload(value: &Value) -> CommandOut {
    let Some(records) = value.as_array() else {
        return Ok(Payload::document(value.clone()).into());
    };
    let mut table = Table::new(vec![
        Column::new("ID").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("ADMISSION").fixed(),
        Column::new("RIGHTS"),
        Column::new("TASKS"),
        Column::new("EXPIRES").fixed(),
    ])
    .empty_message("no operation-mode grants in this workspace");
    for record in records {
        table.add_row(vec![
            string_field(record, "id"),
            string_field(record, "status"),
            string_field(record, "admission"),
            string_list(record, "rights_granted"),
            string_list(record, "task_ids"),
            string_field(record, "expires_at"),
        ]);
    }
    Ok(Payload::list(records.clone(), table).into())
}

fn grant_payload(value: &Value) -> CommandOut {
    if !value.is_object() {
        return Ok(Payload::document(value.clone()).into());
    }
    let mut lines = vec![
        format!("Grant: {}", string_field(value, "id")),
        format!("Status: {}", string_field(value, "status")),
        format!("Admission: {}", string_field(value, "admission")),
        format!("Rights: {}", string_list(value, "rights_granted")),
        format!("Tasks: {}", string_list(value, "task_ids")),
        format!("Expires: {}", string_field(value, "expires_at")),
        format!(
            "Revision: {}",
            value.get("revision").cloned().unwrap_or(Value::Null)
        ),
    ];
    let outcome = string_field(value, "outcome");
    if !outcome.is_empty() {
        lines.push(format!("Outcome: {outcome}"));
    }
    if let Some(coordinators) = value.get("coordinators").and_then(Value::as_array)
        && !coordinators.is_empty()
    {
        lines.push("Coordinators:".to_string());
        for coordinator in coordinators {
            lines.push(format!(
                "  {} {} (remaining children: {})",
                string_field(coordinator, "run_id"),
                string_field(coordinator, "outcome"),
                coordinator
                    .get("remaining_children")
                    .cloned()
                    .unwrap_or(Value::Null)
            ));
        }
    }
    if let Some(limits) = value.get("limits") {
        lines.push(format!(
            "Limits: leaf ceiling {}, preparation due {}s, recovery {} episodes / {} min per task",
            limits.get("leaf_ceiling").cloned().unwrap_or(Value::Null),
            limits
                .get("preparation_due_seconds")
                .cloned()
                .unwrap_or(Value::Null),
            limits
                .get("recovery_episodes_per_task")
                .cloned()
                .unwrap_or(Value::Null),
            limits
                .get("recovery_minutes_per_task")
                .cloned()
                .unwrap_or(Value::Null),
        ));
    }
    Ok(Payload::detail(value.clone(), lines.join("\n")).into())
}

fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn string_list(value: &Value, key: &str) -> String {
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
