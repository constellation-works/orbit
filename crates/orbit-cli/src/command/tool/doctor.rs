use clap::Args;
use orbit_core::{OrbitError, OrbitRuntime};

use crate::command::{Block, CommandOut, Payload};
use crate::output::color::{Domain, Role};
use serde_json::json;

#[derive(Args)]
pub struct ToolDoctorArgs {
    /// Print complete diagnostics for one tool (exact TOOL name)
    pub name: Option<String>,
}

pub(super) fn execute_tool_doctor(runtime: &OrbitRuntime, name: Option<&str>) -> CommandOut {
    use orbit_core::adapter::command::DoctorStatus;

    let results = runtime.doctor()?;
    if let Some(name) = name {
        let row = results
            .iter()
            .find(|row| row.tool_name == name)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!("no tool diagnostic named '{name}'"))
            })?;
        let status = match row.status {
            DoctorStatus::Ok => "ok",
            DoctorStatus::Warning => "warning",
            DoctorStatus::Error => "error",
        };
        return Ok(Payload::detail(
            json!({
                "tool_name": row.tool_name, "status": status, "message": row.message,
            }),
            format!(
                "Tool: {}\nStatus: {status}\nDetails: {}",
                row.tool_name, row.message
            ),
        )
        .into());
    }
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
