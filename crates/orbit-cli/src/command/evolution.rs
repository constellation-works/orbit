//! CLI adapter over the shared evolution registry.
use super::{CommandOut, Execute, Payload, operation_args::Invocation};
use crate::output::{
    payload::Block,
    table::{Column, Table},
};
use clap::Args;
use orbit_common::governance::evolution::EvolutionVerb;
use orbit_core::OrbitRuntime;
use serde_json::Value;

#[derive(Args)]
#[command(about = orbit_common::governance::evolution::EVOLUTION.cli_about)]
pub struct EvolutionCommand {
    #[command(subcommand)]
    pub command: Invocation<EvolutionVerb>,
}
impl Execute for EvolutionCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let Invocation { spec, input } = self.command;
        let value = runtime.run_tool(spec.tool_name, Value::Object(input))?;
        let block = match spec.verb {
            EvolutionVerb::List => {
                let mut table = Table::new(vec![
                    Column::new("ID").fixed(),
                    Column::new("SIGNATURE"),
                    Column::new("STATE"),
                    Column::new("OCCURRENCES"),
                    Column::new("MANDATES"),
                    Column::new("MINUTES"),
                    Column::new("SLOT MINUTES"),
                ])
                .empty_message("no evolution records");
                for row in value["records"].as_array().into_iter().flatten() {
                    table.add_row(
                        [
                            "id",
                            "signature",
                            "state",
                            "occurrence_count",
                            "mandate_count",
                            "minutes",
                            "slot_minutes",
                        ]
                        .iter()
                        .map(|key| display(&row[*key]))
                        .collect(),
                    );
                }
                Block::table(table)
            }
            EvolutionVerb::Add => Block::text(if value["record"].is_null() {
                "Cost recorded.".into()
            } else {
                format!(
                    "Cost recorded for {} ({}).",
                    display(&value["record"]["signature"]),
                    display(&value["record"]["id"])
                )
            }),
            EvolutionVerb::Show => Block::text(format!(
                "{} [{}]\n{}\nOccurrences: {}; minutes: {}; slot minutes: {}",
                display(&value["signature"]),
                display(&value["state"]),
                display(&value["title"]),
                display(&value["occurrence_count"]),
                display(&value["minutes"]),
                display(&value["slot_minutes"])
            )),
            EvolutionVerb::Decline => Block::text(format!(
                "{} suppressed until {}.",
                display(&value["signature"]),
                display(&value["declined_until"])
            )),
        };
        Ok(Payload::blocks(value, vec![block]).into())
    }
}
fn display(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}
