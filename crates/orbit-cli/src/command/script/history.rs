use clap::Args;
use orbit_core::Core;
use orbit_core::app::run::{Command, exec};
use serde_json::Value;

use crate::command::{CommandOut, Payload};
use crate::output::common::table::{Column, Table};

#[derive(Args)]
pub struct ScriptHistoryArgs {
    /// Script name.
    pub name: String,

    /// Most attempts to list, newest first.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
}

impl ScriptHistoryArgs {
    pub(super) fn execute(self, core: &Core) -> CommandOut {
        let executions = exec(core, Command::script_history(&self.name, self.limit))?.value;
        let records: Vec<Value> = executions
            .iter()
            .map(|execution| execution.to_json())
            .collect();
        let mut table = Table::new(vec![
            Column::new("WHEN").fixed(),
            Column::new("RUN").fixed(),
            Column::new("ATTEMPT"),
        ])
        .empty_message("no recorded attempts of this script in this workspace");
        for execution in &executions {
            table.add_row(vec![
                execution
                    .timestamp
                    .map(|at| at.to_rfc3339())
                    .unwrap_or_else(|| "-".to_string()),
                execution.run_id.clone(),
                execution.describe(),
            ]);
        }
        Ok(Payload::list(records, table).into())
    }
}
