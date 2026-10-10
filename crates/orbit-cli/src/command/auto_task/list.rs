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
    /// Also list definitions seeded by a plugin that is switched off in this
    /// workspace or on the host, marked inactive with the reason
    #[arg(long, visible_alias = "all")]
    pub include_inactive_plugins: bool,
}

impl Execute for AutoTaskListArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        // A definition a plugin seeded never fires while that plugin is off
        // in this workspace or on the host, so it is hidden unless asked for.
        let mut listed = runtime.auto_task_listing(self.include_inactive_plugins)?;
        // For one release the deprecated `operation.review_policy =
        // "after-landing"` still enables an unconfigured `delivery-code-review`,
        // so filters and states read the effective value [ORB-13992].
        if self.enabled {
            listed.retain(|l| runtime.auto_task_enabled(&l.definition));
        }
        if self.disabled {
            listed.retain(|l| !runtime.auto_task_enabled(&l.definition));
        }

        // Two ways a listed definition never fires, both reported here so the
        // list never silently implies it is scheduled: its plugin is off (only
        // listed on request), or it is a delivery definition whose resolved
        // owner is another machine and can never be admitted on this host
        // [ORB-12867]. The file stays exactly where it is either way.
        let skips: Vec<Option<String>> = listed
            .iter()
            .map(|l| {
                l.skipped_reason.clone().or_else(|| {
                    orbit_core::application::automation::delivery_ownership_refusal(
                        runtime,
                        &l.definition,
                    )
                    .map(|unadmittable| unadmittable.reason())
                })
            })
            .collect();
        let records: Vec<Value> = listed
            .iter()
            .zip(&skips)
            .map(|(l, skipped)| {
                let mut record = definition_to_json(&l.definition);
                record["skipped_reason"] = match skipped {
                    Some(reason) => Value::String(reason.clone()),
                    None => Value::Null,
                };
                record["plugin_inactive"] = Value::Bool(l.inactive_plugin.is_some());
                record["effective_enabled"] = Value::Bool(runtime.auto_task_enabled(&l.definition));
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
        for (l, skipped) in listed.iter().zip(&skips) {
            let definition = &l.definition;
            let state = if l.inactive_plugin.is_some() {
                "inactive"
            } else if skipped.is_some() {
                "skipped"
            } else if runtime.auto_task_enabled(definition) {
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
        for (l, skipped) in listed.iter().zip(&skips) {
            if let Some(reason) = skipped {
                let label = if l.inactive_plugin.is_some() {
                    "inactive"
                } else {
                    "skipped"
                };
                eprintln!("{label} [{}]: {reason}", l.definition.name);
            }
        }
        Ok(Payload::list(records, table).into())
    }
}
