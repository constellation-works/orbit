use clap::Args;
use orbit_core::Core;
use orbit_core::app::skill::{Command, Outcome, SkillDoctorResult, SkillDoctorStatus, exec};
use serde_json::{Value, json};

use crate::command::{Block, CommandOut, ExecuteCore, Payload};
use crate::output::common::color::{Domain, Role};

#[derive(Args)]
pub struct SkillDoctorArgs {
    /// Print complete diagnostics for one skill (exact SKILL name)
    pub name: Option<String>,
}

impl ExecuteCore for SkillDoctorArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Diagnosed(rows) = exec(
            core,
            Command::Doctor {
                name: self.name.clone(),
            },
        )?
        else {
            unreachable!("skill doctor returned an unrelated outcome");
        };
        if let Some(name) = self.name {
            // The catalog and several client link directories can each report
            // the same skill. Preserve every finding for the selected skill.
            let findings = rows.iter().map(doctor_row_json).collect::<Vec<_>>();
            let mut detail = format!("Skill: {name}");
            for row in &rows {
                detail.push_str(&format!(
                    "\nStatus: {}\nDetails: {}",
                    skill_doctor_status_label(&row.status),
                    row.message
                ));
            }
            return Ok(
                Payload::detail(json!({"skill_id": name, "findings": findings}), detail).into(),
            );
        }
        let values = rows.iter().map(doctor_row_json).collect::<Vec<_>>();

        let mut issues = 0usize;
        use crate::output::common::table::{Column, Table};
        let mut table = Table::new(vec![
            Column::new("SKILL").fixed(),
            Column::new("STATUS").fixed(),
            Column::new("DETAILS"),
        ])
        .empty_message("no file skills installed");
        for row in &rows {
            let status = match row.status {
                SkillDoctorStatus::Ok => "ok",
                SkillDoctorStatus::Warning => "warning",
                SkillDoctorStatus::Error => "ERROR",
            };
            if row.status != SkillDoctorStatus::Ok {
                issues += 1;
            }
            use comfy_table::Cell;
            table.add_row(vec![
                Cell::new(&row.skill_name),
                crate::output::common::color::cell(status, Domain::DoctorStatus),
                Cell::new(&row.message),
            ]);
        }
        let mut blocks = vec![Block::table(table)];
        if issues == 0 {
            blocks.push(Block::notice(format!(
                "\n{}",
                crate::output::common::color::text("All skills healthy.", Role::Ok)
            )));
        } else {
            eprintln!("\n{} issue(s) found.", issues);
        }
        Ok(Payload::blocks(Value::Array(values), blocks).into())
    }
}

fn doctor_row_json(row: &SkillDoctorResult) -> Value {
    json!({
        "skill_id": row.skill_name,
        "status": skill_doctor_status_label(&row.status),
        "message": row.message,
    })
}

fn skill_doctor_status_label(status: &SkillDoctorStatus) -> &'static str {
    match status {
        SkillDoctorStatus::Ok => "ok",
        SkillDoctorStatus::Warning => "warning",
        SkillDoctorStatus::Error => "error",
    }
}
