use clap::Args;
use orbit_core::OrbitRuntime;
use serde_json::Value;

use crate::command::{CommandOut, Execute, Payload};

use super::output::{definition_to_json, schedule_summary};

#[derive(Args)]
pub struct AutoTaskListArgs {
    /// Show only enabled (or, with `--disabled`, only disabled) definitions
    #[arg(long)]
    pub enabled: bool,
    /// Show only disabled definitions
    #[arg(long)]
    pub disabled: bool,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for AutoTaskListArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let mut definitions = runtime.auto_task_list()?;
        if self.enabled {
            definitions.retain(|d| d.enabled);
        }
        if self.disabled {
            definitions.retain(|d| !d.enabled);
        }

        let records: Vec<Value> = definitions.iter().map(definition_to_json).collect();

        use crate::output::table::{Column, Table};
        let mut table = Table::new(vec![
            Column::new("NAME").fixed(),
            Column::new("STATE").fixed().filtered(true),
            Column::new("SCHEDULE").fixed(),
            Column::new("TITLE"),
        ])
        .empty_message("no auto-task definitions");
        for definition in &definitions {
            let state = if definition.enabled {
                "enabled"
            } else {
                "disabled"
            };
            table.add_row(vec![
                definition.name.clone(),
                state.to_string(),
                schedule_summary(definition),
                definition.template.title.clone(),
            ]);
        }
        Ok(Payload::list(records, table).into())
    }
}
