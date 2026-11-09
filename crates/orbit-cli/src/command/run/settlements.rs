use clap::Args;
use orbit_core::OrbitRuntime;
use serde_json::json;

use crate::command::{Block, CommandOut, Execute, Payload};
use crate::parse::parse_since;

use super::format::summarize_error_message;

#[derive(Args)]
#[command(
    after_help = "Lists, newest first, the failure and release settlements that claimed leaves on \
follower hosts sent this owner; `orbit run history` here never lists them. EVIDENCE is the most \
specific class the settlement carried.\n\
Evidence classes, failure accounting, run evidence and the JSON fields: installed Orbit skill \
`references/setup/distributed-drain.md`.\n\
Examples:\n  orbit run settlements\n  orbit run settlements --since 7d\n  \
orbit run settlements --since 2026-10-01T00:00:00Z --no-reconcile --json"
)]
pub struct RunSettlementsArgs {
    /// Only settlements recorded at or after this RFC 3339 time or relative
    /// duration (7d, 24h)
    #[arg(long)]
    pub since: Option<String>,

    /// Report stored claim records as-is, without recovering an interrupted commit
    #[arg(long)]
    pub no_reconcile: bool,
}

impl Execute for RunSettlementsArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let since = self.since.as_deref().map(parse_since).transpose()?;
        let settlements = runtime.leaf_settlements(since, !self.no_reconcile)?;
        let doc = json!({ "settlements": settlements });

        use crate::output::table::{Column, Table};
        let mut table = Table::new(vec![
            Column::new("SETTLED_AT").fixed(),
            Column::new("TASK").fixed(),
            Column::new("MACHINE").fixed(),
            Column::new("LEAF_RUN").fixed(),
            Column::new("KIND").fixed(),
            Column::new("EVIDENCE").fixed(),
            Column::new("CLASS").fixed(),
            Column::new("REASON"),
        ])
        .empty_message("no claimed-leaf settlements recorded");
        for settlement in &settlements {
            use comfy_table::Cell;
            let kind = doc_label(&settlement.kind);
            let class = doc_label(&settlement.failure_class);
            table.add_row(vec![
                Cell::new(&settlement.settled_at),
                Cell::new(&settlement.task_id),
                Cell::new(
                    settlement
                        .machine_name
                        .as_deref()
                        .unwrap_or(&settlement.machine_id),
                ),
                Cell::new(settlement.leaf_run_id.as_deref().unwrap_or("-")),
                Cell::new(kind),
                Cell::new(settlement.evidence),
                Cell::new(class),
                Cell::new(summarize_error_message(settlement.reason.as_deref())),
            ]);
        }
        Ok(Payload::blocks(doc, vec![Block::table(table)]).into())
    }
}

/// A wire enum's JSON name, or `-` when absent.
fn doc_label<T: serde::Serialize>(value: &Option<T>) -> String {
    orbit_common::protocol::serde_string_label(value).unwrap_or_else(|| "-".to_string())
}
