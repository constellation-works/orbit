use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::application::auto_tasks::AutoTaskSkip;
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
    /// Also list definitions seeded by a plugin that is switched off in this
    /// workspace or on the host, marked inactive with the reason
    #[arg(long, visible_alias = "all")]
    pub include_inactive_plugins: bool,
}

impl Execute for AutoTaskListArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let mut rows = runtime
            .auto_task_listing_state(self.include_inactive_plugins, chrono::Utc::now())?
            .rows;
        if self.enabled {
            rows.retain(|row| row.effective_enabled);
        }
        if self.disabled {
            rows.retain(|row| !row.effective_enabled);
        }

        let records: Vec<Value> = rows
            .iter()
            .map(|row| {
                let mut record = definition_to_json(&row.listed.definition);
                record["skipped_reason"] = match &row.skipped_reason {
                    Some(reason) => Value::String(reason.clone()),
                    None => Value::Null,
                };
                record["plugin_inactive"] =
                    Value::Bool(row.skip == Some(AutoTaskSkip::PluginInactive));
                record["effective_enabled"] = Value::Bool(row.effective_enabled);
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
        for row in &rows {
            let definition = &row.listed.definition;
            let state = match row.skip {
                Some(AutoTaskSkip::PluginInactive) => "inactive",
                Some(AutoTaskSkip::NotOwnedHere) => "skipped",
                None if row.effective_enabled => "enabled",
                None => "disabled",
            };
            table.add_row(vec![
                definition.name.clone(),
                state.to_string(),
                schedule_summary(definition),
                definition.template.title.clone(),
            ]);
        }
        for row in &rows {
            if let Some(reason) = &row.skipped_reason {
                let label = match row.skip {
                    Some(AutoTaskSkip::PluginInactive) => "inactive",
                    _ => "skipped",
                };
                table = table.trailing_notice(format!(
                    "{label} [{}]: {reason}",
                    row.listed.definition.name
                ));
            }
        }
        Ok(Payload::list(records, table).into())
    }
}
