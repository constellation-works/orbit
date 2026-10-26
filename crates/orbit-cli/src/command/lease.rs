//! Registry-derived path-lease CLI with standard structured and human output.

use clap::Args;
use orbit_common::governance::lease::LeaseVerb;
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};
use std::fmt::Write as _;

use super::operation_args::Invocation;
use crate::command::{Block, CommandOut, Execute, Payload};
use crate::output::table::{Column, Table};

#[derive(Args)]
#[command(about = orbit_common::governance::lease::LEASE.cli_about)]
pub struct LeaseCommand {
    #[command(subcommand)]
    pub command: Invocation<LeaseVerb>,
}

impl Execute for LeaseCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, mut input } = self.command;
        if let Some(raw) = input
            .remove("ttl")
            .and_then(|value| value.as_str().map(str::to_owned))
        {
            input.insert(
                "ttl_seconds".into(),
                json!(super::locks::reservation_ttl_seconds(&raw)?),
            );
        }
        if input.remove("hard").and_then(|value| value.as_bool()) == Some(true) {
            input.insert("mode".into(), json!("hard"));
        }
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        let exit_code = if spec.verb == LeaseVerb::Request && value["granted"] == false {
            3
        } else {
            0
        };
        let blocks = match spec.verb {
            LeaseVerb::Request if value["granted"] == false => vec![
                Block::text("Lease refused: overlapping holders"),
                Block::table(lease_holders_table(&value["conflicts"])),
            ],
            LeaseVerb::Request | LeaseVerb::Renew | LeaseVerb::Show => {
                let mut blocks = vec![Block::text(lease_detail(&value["lease"]))];
                if let Some(notice) = value["notice"].as_str() {
                    blocks.push(Block::text(notice));
                }
                if value["overlaps"]
                    .as_array()
                    .is_some_and(|rows| !rows.is_empty())
                {
                    blocks.push(Block::table(lease_holders_table(&value["overlaps"])));
                }
                blocks
            }
            LeaseVerb::Release => vec![Block::text(if value["released"] == true {
                format!("Released {}", lease_text(&value["lease"]["lease_id"]))
            } else {
                "Lease was unknown or already ended".into()
            })],
            LeaseVerb::List => {
                let mut table = Table::new(vec![
                    Column::new("LEASE").fixed(),
                    Column::new("MODE").fixed(),
                    Column::new("PRINCIPAL").fixed(),
                    Column::new("PRIORITY").fixed(),
                    Column::new("EXPIRES").fixed(),
                    Column::new("SELECTORS"),
                ])
                .keep_all_columns()
                .empty_message("no active leases");
                for lease in value["leases"].as_array().into_iter().flatten() {
                    table.add_row(vec![
                        lease_text(&lease["lease_id"]),
                        lease_text(&lease["mode"]),
                        lease_text(&lease["principal"]),
                        lease_text(&lease["priority"]),
                        lease_text(&lease["expires_at"]),
                        lease_selectors(&lease["selectors"]),
                    ]);
                }
                vec![Block::table(table)]
            }
            LeaseVerb::Check => {
                let mut blocks = vec![Block::text(format!(
                    "Caller lease active: {}",
                    value["lease_active"]
                ))];
                for path in value["paths"].as_array().into_iter().flatten() {
                    blocks.push(Block::text(format!(
                        "{}: covered={}, unlocked={}",
                        lease_text(&path["path"]),
                        path["covered"],
                        path["unlocked"]
                    )));
                    blocks.push(Block::table(lease_holders_table(&path["holders"])));
                }
                blocks
            }
        };
        Ok(Payload::blocks(value, blocks)
            .with_exit_code(exit_code)
            .into())
    }
}

fn lease_detail(lease: &Value) -> String {
    let mut out = format!(
        "{} ({})\nMode: {}\nPrincipal: {}\nPriority: {}\nExpires: {}\nSelectors:",
        lease_text(&lease["lease_id"]),
        lease_text(&lease["state"]),
        lease_text(&lease["mode"]),
        lease_text(&lease["principal"]),
        lease_text(&lease["priority"]),
        lease_text(&lease["expires_at"])
    );
    for selector in lease["selectors"].as_array().into_iter().flatten() {
        let _ = write!(out, "\n  - {}", lease_text(selector));
    }
    out
}

fn lease_holders_table(holders: &Value) -> Table {
    let mut table = Table::new(vec![
        Column::new("SELECTOR"),
        Column::new("HELD BY").fixed(),
        Column::new("HOLDER").fixed(),
        Column::new("MODE").fixed(),
        Column::new("PRINCIPAL").fixed(),
    ])
    .keep_all_columns()
    .empty_message("no overlapping holders");
    for holder in holders.as_array().into_iter().flatten() {
        table.add_row(vec![
            lease_text(&holder["file"]),
            lease_text(&holder["held_by"]),
            lease_text(&holder["held_by_id"]),
            lease_text(&holder["mode"]),
            lease_text(&holder["principal"]),
        ]);
    }
    table
}

fn lease_text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}
fn lease_selectors(value: &Value) -> String {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}
