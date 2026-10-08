use std::path::Path;

use chrono::{DateTime, Local, SecondsFormat};
use clap::Args;
use comfy_table::Cell;
use orbit_cmd::registry_routines::{routine_statuses, routine_statuses_for_workspace};
use orbit_core::application::routines::RetiredRoutine;
use serde_json::json;

use crate::command::{CommandOut, Payload};
use crate::output::table::{Column, Table};

#[derive(Args)]
pub struct RoutineListArgs {
    /// Output as JSON.
    #[arg(long)]
    pub json: bool,
    /// Also list routines seeded by a plugin that is switched off in their
    /// workspace or on the host, marked inactive with the reason
    #[arg(long, visible_alias = "all")]
    pub include_inactive_plugins: bool,
}

impl RoutineListArgs {
    pub fn execute_without_runtime(
        self,
        global_root: &Path,
        workspace_selector: Option<&str>,
    ) -> CommandOut {
        let report = match workspace_selector {
            Some(selector) => routine_statuses_for_workspace(global_root, selector)?,
            None => routine_statuses(global_root)?,
        };
        // A routine a plugin seeded never fires while that plugin is off where
        // it lives, so it is hidden unless asked for.
        let retired: Vec<_> = report
            .listed_retired(self.include_inactive_plugins)
            .collect();

        let statuses: Vec<_> = report
            .statuses
            .iter()
            .map(|status| {
                json!({
                    "name": status.routine.definition.name,
                    "source": status.routine.source_workspace,
                    "origin": status.routine.origin.as_str(),
                    "target": status.routine.definition.target.as_ref_string(),
                    "enabled": status.routine.definition.enabled,
                    "paused_at": status.paused_at,
                    "effective": status.effective(),
                    "cron": status.routine.definition.trigger.cron,
                    "next_due": status.next_due,
                    "last_fire": status.last_fire.as_ref().map(|fire| json!({
                        "slot": fire.slot,
                        "attempt": fire.attempt,
                        "state": fire.state.as_str(),
                        "run_id": fire.run_id,
                    })),
                })
            })
            .collect();
        let doc = json!({
            "machine_name": report.machine_name,
            "machine_id": report.machine_id,
            "routines": statuses,
            "retired": retired.iter().map(|routine| json!({
                "name": routine.name,
                "source": routine.source_workspace,
                "origin": routine.origin.as_str(),
                "path": routine.path.display().to_string(),
                "target": format!("job:{}", routine.job),
                "reason": routine.reason,
                "plugin_inactive": routine.skipped,
            })).collect::<Vec<_>>(),
            "owner_only": report.owner_only.iter().map(|owned| json!({
                "name": owned.routine.definition.name,
                "source": owned.routine.source_workspace,
                "origin": owned.routine.origin.as_str(),
                "path": owned.routine.path.display().to_string(),
                "target": owned.routine.definition.target.as_ref_string(),
                "enabled": owned.routine.definition.enabled,
                "owner_machine": owned.owner_machine,
                "reason": owned.reason,
            })).collect::<Vec<_>>(),
            "load_errors": report.load_errors.iter().map(|e| json!({
                "source_workspace": e.source_workspace,
                "path": e.path.as_ref().map(|p| p.display().to_string()),
                "message": e.message,
            })).collect::<Vec<_>>(),
        });

        // `orbit routine show <name>` prints a routine's full definition.
        let mut table = Table::new(vec![
            Column::new("NAME").fixed(),
            Column::new("SOURCE"),
            Column::new("ORIGIN").fixed(),
            Column::new("ENABLED").fixed(),
            Column::new("PAUSED").fixed(),
            Column::new("NEXT DUE").fixed(),
            Column::new("LAST FIRE").fixed(),
        ])
        .empty_message(match workspace_selector {
            Some(selector) => format!("no routines found in workspace '{selector}'"),
            None => format!(
                "no routines found (host {}); register an owner checkout that defines \
                 .orbit/routines/*.yaml",
                report.machine_name
            ),
        });
        for status in &report.statuses {
            let last_fire = status
                .last_fire
                .as_ref()
                .map(|fire| format!("{} @ {}", fire.state.as_str(), host_local(&fire.slot)))
                .unwrap_or_else(|| "—".to_string());
            table.add_row(vec![
                Cell::new(&status.routine.definition.name),
                Cell::new(&status.routine.source_workspace),
                Cell::new(status.routine.origin.as_str()),
                Cell::new(if status.routine.definition.enabled {
                    "yes"
                } else {
                    "no"
                }),
                Cell::new(if status.paused_at.is_some() {
                    "yes"
                } else {
                    "no"
                }),
                Cell::new(
                    status
                        .next_due
                        .as_deref()
                        .map_or_else(|| "—".to_string(), host_local),
                ),
                Cell::new(last_fire),
            ]);
        }
        // A definition targeting a retired job, or one whose plugin is off
        // (on request), is listed so the operator can see it exists, but it
        // has no schedule state of its own.
        let label = |routine: &RetiredRoutine| {
            if routine.skipped {
                "inactive"
            } else {
                "retired"
            }
        };
        for routine in &retired {
            table.add_row(vec![
                Cell::new(&routine.name),
                Cell::new(&routine.source_workspace),
                Cell::new(routine.origin.as_str()),
                Cell::new("—"),
                Cell::new("—"),
                Cell::new(label(routine)),
                Cell::new("—"),
            ]);
        }
        // A replica's other definitions are its owner's to schedule.
        for owned in &report.owner_only {
            table.add_row(vec![
                Cell::new(&owned.routine.definition.name),
                Cell::new(&owned.routine.source_workspace),
                Cell::new(owned.routine.origin.as_str()),
                Cell::new(if owned.routine.definition.enabled {
                    "yes"
                } else {
                    "no"
                }),
                Cell::new("—"),
                Cell::new("owner-only"),
                Cell::new("—"),
            ]);
        }
        // Context about where the list came from, not a record in it (spec §5).
        eprintln!("host: {}", report.machine_name);
        for owned in &report.owner_only {
            eprintln!(
                "owner-only [{}] ({}): {}",
                owned.routine.source_workspace,
                owned.routine.path.display(),
                owned.reason
            );
        }
        for routine in &retired {
            eprintln!(
                "{} [{}] ({}): {}",
                label(routine),
                routine.source_workspace,
                routine.path.display(),
                routine.reason
            );
        }
        for error in &report.load_errors {
            let path = error
                .path
                .as_ref()
                .map(|path| format!(" ({})", path.display()))
                .unwrap_or_default();
            eprintln!(
                "load error [{}]{}: {}",
                error.source_workspace, path, error.message
            );
        }
        Ok(Payload::detail_table(doc, table).into())
    }
}

/// An RFC 3339 instant in the host's zone, to the second, so the NEXT DUE and
/// LAST FIRE cells of one row read in the same zone whichever zone each source
/// recorded. Text that is not an RFC 3339 instant is shown as recorded.
fn host_local(raw: &str) -> String {
    DateTime::parse_from_rfc3339(raw).map_or_else(
        |_| raw.to_string(),
        |instant| {
            instant
                .with_timezone(&Local)
                .to_rfc3339_opts(SecondsFormat::Secs, false)
        },
    )
}
