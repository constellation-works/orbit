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

        // A definition a plugin seeded stops firing when that plugin is
        // disabled. It is still listed — the file is still there — with the
        // reason, so the list never silently implies it is scheduled.
        let skips: Vec<Option<String>> = definitions
            .iter()
            .map(|definition| runtime.auto_task_skip_reason(definition))
            .collect();
        let records: Vec<Value> = definitions
            .iter()
            .zip(&skips)
            .map(|(definition, skipped)| {
                let mut record = definition_to_json(definition);
                record["skipped_reason"] = match skipped {
                    Some(reason) => Value::String(reason.clone()),
                    None => Value::Null,
                };
                record
            })
            .collect();

        use crate::output::table::{Column, Table};
        let mut table = Table::new(vec![
            Column::new("NAME").fixed(),
            Column::new("STATE").fixed().filtered(true),
            Column::new("SCHEDULE").fixed(),
            Column::new("TITLE"),
        ])
        .empty_message("no auto-task definitions");
        for (definition, skipped) in definitions.iter().zip(&skips) {
            let state = if skipped.is_some() {
                "skipped"
            } else if definition.enabled {
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
        for (definition, skipped) in definitions.iter().zip(&skips) {
            if let Some(reason) = skipped {
                eprintln!("skipped [{}]: {reason}", definition.name);
            }
        }
        Ok(Payload::list(records, table).into())
    }
}
