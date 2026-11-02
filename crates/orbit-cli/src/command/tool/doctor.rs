use orbit_core::OrbitRuntime;

use crate::command::{Block, CommandOut, Payload};
use crate::output::color::{Domain, Role};
use serde_json::json;

pub(super) fn execute_doctor(runtime: &OrbitRuntime) -> CommandOut {
    use orbit_core::adapter::command::DoctorStatus;

    let results = runtime.doctor()?;
    let mut issues = 0;

    use crate::output::table::{Column, Table};
    let mut table = Table::new(vec![
        Column::new("TOOL").fixed(),
        Column::new("STATUS").fixed(),
        Column::new("DETAILS"),
    ])
    .empty_message("no tools registered");
    for r in &results {
        let status_str = match r.status {
            DoctorStatus::Ok => "ok",
            DoctorStatus::Warning => "warning",
            DoctorStatus::Error => "ERROR",
        };
        if r.status != DoctorStatus::Ok {
            issues += 1;
        }
        use comfy_table::Cell;
        table.add_row(vec![
            Cell::new(&r.tool_name),
            crate::output::color::cell(status_str, Domain::DoctorStatus),
            Cell::new(&r.message),
        ]);
    }
    let records = results
        .iter()
        .map(|r| {
            json!({
                "tool_name": r.tool_name,
                "status": match r.status {
                    DoctorStatus::Ok => "ok",
                    DoctorStatus::Warning => "warning",
                    DoctorStatus::Error => "error",
                },
                "message": r.message,
            })
        })
        .collect::<Vec<_>>();

    let mut blocks = vec![Block::table(table)];
    if issues == 0 {
        blocks.push(Block::notice(format!(
            "\n{}",
            crate::output::color::text("All tools healthy.", Role::Ok)
        )));
    } else {
        eprintln!("\n{} issue(s) found.", issues);
    }

    Ok(Payload::blocks(serde_json::Value::Array(records), blocks).into())
}
