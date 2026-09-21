use orbit_core::OrbitRuntime;
use serde_json::json;

use crate::command::{Block, CommandOut, Payload};
use crate::output::color::{Domain, Role};

pub(super) fn execute_doctor(runtime: &OrbitRuntime) -> CommandOut {
    let results = runtime.plugin_doctor()?;

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
    let records = results
        .iter()
        .map(|result| {
            json!({
                "plugin": result.plugin,
                "status": result.status.as_str(),
                "message": result.message,
            })
        })
        .collect::<Vec<_>>();

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
