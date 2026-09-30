use orbit_core::OrbitRuntime;
use serde_json::json;

use crate::command::{Block, CommandOut, Payload};
use crate::output::color::{Domain, Role};

pub(super) fn execute_doctor(runtime: &OrbitRuntime) -> CommandOut {
    let results = runtime.plugin_doctor()?;
    let stale_callbacks = runtime.stale_plugin_callback_session_count()?;
    let legacy_callback_identity = runtime.legacy_plugin_callback_identity()?;

    use crate::output::table::{Column, Table};
    use comfy_table::Cell;
    let mut table = Table::new(vec![
        Column::new("PLUGIN").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("NEXT STEP"),
    ])
    .empty_message("no plugins installed or pinned");
    let mut issues = 0;
    let mut switched_off = 0;
    for result in &results {
        // A plugin switched off in this workspace is the operator's choice:
        // shown, so the state stays visible, but not counted as a finding.
        if result.intentional {
            switched_off += 1;
        } else if !result.message.is_empty() {
            issues += 1;
        }
        table.add_row(vec![
            Cell::new(&result.plugin),
            crate::output::color::cell(result.status.as_str(), Domain::JobState),
            Cell::new(&result.message),
        ]);
    }
    let mut records = results
        .iter()
        .map(|result| {
            json!({
                "plugin": result.plugin,
                "status": result.status.as_str(),
                "message": result.message,
                "intentional": result.intentional,
            })
        })
        .collect::<Vec<_>>();
    if stale_callbacks > 0 {
        issues += 1;
        let message = format!(
            "{stale_callbacks} stale plugin callback session record(s); they will be removed when a plugin backend starts"
        );
        table.add_row(vec![
            Cell::new("plugin callbacks"),
            crate::output::color::cell("inactive", Domain::JobState),
            Cell::new(&message),
        ]);
        records.push(json!({
            "plugin": "plugin callbacks",
            "status": "inactive",
            "message": message,
        }));
    }

    // A host that still honours the retired credential is running one release
    // of compatibility, not a supported configuration: the environment token
    // and process ancestry are what `setsid` escaped, and they are removed in
    // the next release [ORB-12841].
    if legacy_callback_identity {
        issues += 1;
        let message = "plugin.legacy_callback_identity is on: the retired environment token and \
                       process ancestry still identify a plugin callback. It is removed in the \
                       next release — clear the key and make sure every backend keeps file \
                       descriptor 3 open across `setsid`, `exec` and any wrapper script"
            .to_string();
        table.add_row(vec![
            Cell::new("plugin callbacks"),
            crate::output::color::cell("inactive", Domain::JobState),
            Cell::new(&message),
        ]);
        records.push(json!({
            "plugin": "plugin callbacks",
            "status": "inactive",
            "message": message,
        }));
    }

    let mut blocks = vec![Block::table(table)];
    if issues == 0 {
        let summary = if switched_off == 0 {
            "Every plugin is serving its tools.".to_string()
        } else {
            format!(
                "Every plugin enabled in this workspace is serving its tools ({switched_off} \
                 switched off here)."
            )
        };
        // The blank line separates the summary from the table; with no rows
        // there is nothing above it to separate from.
        let separator = if results.is_empty() { "" } else { "\n" };
        blocks.push(Block::text(format!(
            "{separator}{}",
            crate::output::color::text(&summary, Role::Ok)
        )));
    } else {
        eprintln!("\n{issues} plugin(s) need attention.");
    }
    Ok(Payload::blocks(serde_json::Value::Array(records), blocks)
        .with_exit_code(i32::from(issues > 0))
        .into())
}
