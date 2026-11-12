use orbit_cmd::{WorkspaceDoctorResult, WorkspaceDoctorStatus};
use orbit_core::{DOCTOR_FINDINGS_MESSAGE_PREFIX, OrbitError};
use serde_json::Value;

use crate::command::{Block, CommandOut, Payload};
use crate::output::color::{Domain, Role};

/// Render the checks as `orbit doctor` reports them: a table sorted by
/// severity, the JSON rows, and a nonzero exit when any check failed.
pub(super) fn report_output(
    mut results: Vec<WorkspaceDoctorResult>,
    problems: bool,
    check: Option<&str>,
) -> CommandOut {
    if let Some(check) = check {
        let row = results
            .iter()
            .find(|row| row.check_name == check)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "no workspace diagnostic named '{check}'; run `orbit doctor --json` to list valid check names"
                ))
            })?;
        let failed = row.status == WorkspaceDoctorStatus::Error;
        let mut payload = Payload::detail(
            orbit_cmd::doctor_row_json(row),
            format!(
                "Check: {}\nStatus: {}\nDetails: {}",
                row.check_name,
                status_label(row.status),
                human_detail(row)
            ),
        )
        .with_exit_code(i32::from(failed));
        if failed {
            payload =
                payload.with_audit_message(findings_audit_message(std::slice::from_ref(row), 0));
        }
        return Ok(payload.into());
    }
    let failures = results
        .iter()
        .filter(|row| row.status == WorkspaceDoctorStatus::Error)
        .count();
    let warnings = results
        .iter()
        .filter(|row| row.status == WorkspaceDoctorStatus::Warning)
        .count();

    let values = results
        .iter()
        .map(orbit_cmd::doctor_row_json)
        .collect::<Vec<_>>();

    let summary = format!("{failures} failure(s), {warnings} warning(s).");
    eprintln!("{summary}");
    let mut blocks = Vec::new();

    results.sort_by_key(|row| match row.status {
        WorkspaceDoctorStatus::Error => 0,
        WorkspaceDoctorStatus::Warning => 1,
        WorkspaceDoctorStatus::Ok => 2,
        WorkspaceDoctorStatus::Info => 3,
        WorkspaceDoctorStatus::Skipped => 4,
    });

    if problems {
        results.retain(|row| {
            matches!(
                row.status,
                WorkspaceDoctorStatus::Error | WorkspaceDoctorStatus::Warning
            )
        });
    }

    {
        use crate::output::table::{Column, Table};
        let mut table = Table::new(vec![
            Column::new("CHECK").fixed(),
            Column::new("STATUS").fixed(),
            Column::new("DETAILS"),
        ])
        .empty_message(if problems {
            "no problems found"
        } else {
            "no workspace checks ran"
        });
        for row in &results {
            use comfy_table::Cell;
            table.add_row(vec![
                Cell::new(&row.check_name),
                crate::output::color::cell(status_label(row.status), Domain::DoctorStatus),
                Cell::new(human_detail(row).replace('\n', " | ")),
            ]);
        }
        blocks.push(Block::table(table));
        blocks.push(Block::DoctorFindings(results.clone()));

        if failures == 0 && warnings == 0 {
            blocks.push(Block::notice(format!(
                "\n{}",
                crate::output::color::text("Workspace healthy.", Role::Ok)
            )));
        }
    }

    // Unlike `skill doctor` / `tool doctor`, a failed check exits nonzero
    // so unattended callers (cron, CI, systemd) can alert on it.
    let exit_code = i32::from(failures > 0);
    let mut payload = Payload::blocks(Value::Array(values), blocks).with_exit_code(exit_code);
    if failures > 0 {
        payload = payload.with_audit_message(findings_audit_message(&results, warnings));
    }
    Ok(payload.into())
}

/// The audit message for a completed run that reported failed checks, e.g.
/// `doctor reported findings: 1 failure (review), 3 warnings`. The prefix is
/// what lets the incident classifier tell this verdict from a crash.
fn findings_audit_message(results: &[WorkspaceDoctorResult], warnings: usize) -> String {
    let failed = results
        .iter()
        .filter(|row| row.status == WorkspaceDoctorStatus::Error)
        .map(|row| row.check_name.as_str())
        .collect::<Vec<_>>();
    format!(
        "{DOCTOR_FINDINGS_MESSAGE_PREFIX}{} {} ({}), {warnings} {}",
        failed.len(),
        if failed.len() == 1 {
            "failure"
        } else {
            "failures"
        },
        failed.join(", "),
        if warnings == 1 { "warning" } else { "warnings" },
    )
}

pub(super) fn status_label(status: WorkspaceDoctorStatus) -> &'static str {
    match status {
        WorkspaceDoctorStatus::Ok => "ok",
        WorkspaceDoctorStatus::Warning => "warning",
        WorkspaceDoctorStatus::Error => "ERROR",
        WorkspaceDoctorStatus::Skipped => "skipped",
        WorkspaceDoctorStatus::Info => "info",
    }
}

fn human_detail(row: &WorkspaceDoctorResult) -> String {
    let mut detail = row.remediation.as_ref().map_or_else(
        || row.message.clone(),
        |remediation| format!("{}\nAction: {remediation}", row.message),
    );
    if row.duration_ms > 1000 {
        detail.push_str(&format!(" ({:.2} s)", row.duration_ms as f64 / 1000.0));
    }
    detail
}
