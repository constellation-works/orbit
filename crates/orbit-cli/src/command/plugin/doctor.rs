use orbit_core::OrbitRuntime;
use serde_json::json;

use crate::command::{Block, CommandOut, Payload};
use crate::output::color::{Domain, Role};

pub(super) fn execute_doctor(runtime: &OrbitRuntime) -> CommandOut {
    let results = runtime.plugin_doctor()?;
    let stale_callbacks = runtime.stale_plugin_callback_session_count()?;

    use crate::output::table::{Column, Table};
    use comfy_table::Cell;
    let mut table = Table::new(vec![
        Column::new("PLUGIN").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("NEXT STEP"),
    ])
    .empty_message("no plugins installed or pinned");
    let mut issues = 0;
    for result in &results {
        if !result.message.is_empty() {
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

    let mut blocks = vec![Block::table(table)];
    if issues == 0 {
        blocks.push(Block::text(format!(
            "\n{}",
            crate::output::color::text("Every plugin is serving its tools.", Role::Ok)
        )));
    } else {
        eprintln!("\n{issues} plugin(s) need attention.");
    }
    Ok(Payload::blocks(serde_json::Value::Array(records), blocks).into())
}
