use clap::Args;
use orbit_cmd::{
    DoctorCommands, OrphanTaskStoreRemoval, WorkspaceDoctorResult, WorkspaceDoctorStatus,
};
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};

use crate::command::{Block, CommandOut, Execute, Payload};
use crate::output::color::{Domain, Role};

/// `orbit doctor` — workspace-level self-diagnostics [ORB-10005].
#[derive(Args)]
#[command(about = "Diagnose workspace health (config, database, disk, indexes, locks, runs)")]
pub struct DoctorCommand {
    /// Emit machine-readable JSON instead of the table.
    #[arg(long)]
    pub json: bool,

    /// Remove lock files whose recorded holder process is dead before diagnosing the workspace.
    #[arg(long)]
    pub fix_stale_locks: bool,

    /// Release only task reservations whose owner/task state is conclusively inactive.
    #[arg(long)]
    pub fix_stale_task_locks: bool,

    /// Remove retired graph state from this worktree and the shared workspace.
    #[arg(long)]
    pub remove_graph: bool,

    /// Retire deprecated skills, jobs, activities, auto-tasks, and routines that Orbit itself wrote. Locally modified ones are preserved, not deleted.
    #[arg(long)]
    pub fix_stale_artifacts: bool,

    /// Remove known retired `spec.backend` values (`http`, `auto`) from schemaVersion 2 agent-loop activities. Unknown backends and unrelated parse failures are left untouched.
    #[arg(long)]
    pub fix_retired_activity_backends: bool,

    /// Delete empty unclaimed task-store partitions, and populated partitions whose bound checkout is confirmed absent (including their task bundles). Unowned or unreachable populated partitions are never touched. Requires --confirm.
    #[arg(long)]
    pub fix_orphan_task_stores: bool,

    /// Confirm a destructive repair. Required by --fix-orphan-task-stores, which deletes partition directories and their task bundles.
    #[arg(long)]
    pub confirm: bool,
}

impl Execute for DoctorCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let mut results = Vec::new();
        if self.fix_stale_locks {
            let removed = runtime.remove_stale_lock_files()?;
            eprintln!("Removed {removed} stale lock file(s).");
            results.push(WorkspaceDoctorResult {
                check_name: "fix-stale-locks".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message: format!("Removed {removed} stale lock file(s)."),
                remediation: None,
            });
        }
        if self.fix_stale_task_locks {
            let released = runtime.clear_stale_task_reservations()?;
            eprintln!("Released {released} stale task reservation(s).");
            results.push(WorkspaceDoctorResult {
                check_name: "fix-stale-task-locks".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message: format!("Released {released} stale task reservation(s)."),
                remediation: None,
            });
        }
        if self.remove_graph {
            let removed = runtime.remove_retired_graph_state()?;
            eprintln!("Removed {removed} retired graph location(s).");
            results.push(WorkspaceDoctorResult {
                check_name: "remove-graph".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message: format!("Removed {removed} retired graph location(s)."),
                remediation: None,
            });
        }
        if self.fix_stale_artifacts {
            let removed = runtime.remove_stale_definition_artifacts()?;
            eprintln!("Retired {removed} deprecated definition artifact(s).");
            results.push(WorkspaceDoctorResult {
                check_name: "fix-stale-artifacts".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message: format!("Retired {removed} deprecated definition artifact(s)."),
                remediation: None,
            });
        }
        if self.fix_retired_activity_backends {
            let report = runtime.repair_retired_activity_backends()?;
            eprintln!(
                "Removed retired spec.backend from {} activity file(s).",
                report.repaired.len()
            );
            for skipped in &report.skipped {
                eprintln!(
                    "Left untouched {}: {}",
                    skipped.path.display(),
                    skipped.reason
                );
            }
            results.push(WorkspaceDoctorResult {
                check_name: "fix-retired-activity-backends".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message: format!(
                    "Removed retired spec.backend from {} activity file(s).",
                    report.repaired.len()
                ),
                remediation: None,
            });
        }
        if self.fix_orphan_task_stores {
            if !self.confirm {
                return Err(orbit_core::OrbitError::InvalidInput(
                    "--fix-orphan-task-stores deletes task-store partition directories and their task bundles. Pass --confirm to proceed."
                        .to_string(),
                ));
            }
            let removed = runtime.remove_orphan_task_stores()?;
            let message = orphan_task_store_removal_message(&removed);
            eprintln!("{message}");
            results.push(WorkspaceDoctorResult {
                check_name: "fix-orphan-task-stores".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message,
                remediation: None,
            });
        }
        results.extend(runtime.doctor_workspace()?);
        // Machine-global rows, composed here rather than in `doctor_workspace`:
        // `orbit-cmd` does not know about MCP and must not learn, and this is
        // the one crate that already assembles both [ORB-11053].
        results.extend(caller_authorization_rows());
        results.push(clock_unit_row());
        let failures = results
            .iter()
            .filter(|row| row.status == WorkspaceDoctorStatus::Error)
            .count();
        let warnings = results
            .iter()
            .filter(|row| row.status == WorkspaceDoctorStatus::Warning)
            .count();

        let values = results.iter().map(doctor_row_json).collect::<Vec<_>>();
        let mut blocks = Vec::new();
        {
            use crate::output::table::{Column, Table};
            let mut table = Table::new(vec![
                Column::new("CHECK").fixed(),
                Column::new("STATUS").fixed(),
                Column::new("DETAILS"),
            ])
            .empty_message("no workspace checks ran");
            for row in &results {
                use comfy_table::Cell;
                table.add_row(vec![
                    Cell::new(&row.check_name),
                    crate::output::color::cell(status_label(row.status), Domain::DoctorStatus),
                    Cell::new(human_detail(row).replace('\n', " | ")),
                ]);
            }
            blocks.push(Block::table(table));

            if failures == 0 && warnings == 0 {
                blocks.push(Block::text(format!(
                    "\n{}",
                    crate::output::color::text("Workspace healthy.", Role::Ok)
                )));
            } else {
                eprintln!("\n{failures} failure(s), {warnings} warning(s).");
            }
        }

        // Unlike `skill doctor` / `tool doctor`, a failed check exits nonzero
        // so unattended callers (cron, CI, systemd) can alert on it.
        let exit_code = i32::from(failures > 0);
        Ok(Payload::blocks(Value::Array(values), blocks)
            .with_exit_code(exit_code)
            .into())
    }
}

/// Whether this machine's MCP caller authorization is in a state an operator
/// would have chosen [ORB-11053].
///
/// Two rows, because they are two different gaps. The first is the Tier 1 one:
/// a machine that serves SSH sessions and has declared nothing about who may
/// call it. The second is the Tier 2 one: an `operator` grant — the strongest
/// thing this file can say — resting on a name any caller could type.
///
/// Both are warnings, never errors. Tier 2 is opt-in, and a destination that
/// deliberately runs Tier 1 alone is a documented configuration with a weaker
/// guarantee, not a broken one. A file that does not *load* is different: it
/// refuses every remote session, so it fails.
fn caller_authorization_rows() -> Vec<WorkspaceDoctorResult> {
    let Ok(home) = orbit_common::fs::path::home_dir() else {
        return Vec::new();
    };
    let health = orbit_mcp::inspect_caller_authorization(
        &home.join(".orbit"),
        &home.join(".ssh/authorized_keys"),
    );
    let file = health.path.display().to_string();
    let callers = if let Some(defect) = &health.defect {
        WorkspaceDoctorResult {
            check_name: "mcp-callers".to_string(),
            status: WorkspaceDoctorStatus::Error,
            // The defect already names the file, so the row does not repeat it.
            message: format!("the MCP callers file does not load: {defect}"),
            remediation: Some(
                "Every remote-originated MCP session is refused until this file parses. Fix the \
                 defect named above, then rerun `orbit mcp callers list`."
                    .to_string(),
            ),
        }
    } else if health.present {
        WorkspaceDoctorResult {
            check_name: "mcp-callers".to_string(),
            status: WorkspaceDoctorStatus::Ok,
            message: format!("{file} declares {} caller(s)", health.row_count),
            remediation: None,
        }
    } else if health.serves_ssh {
        WorkspaceDoctorResult {
            check_name: "mcp-callers".to_string(),
            status: WorkspaceDoctorStatus::Warning,
            message: format!(
                "this machine accepts SSH logins but has no {file}, so remote-originated MCP \
                 sessions are served agent capabilities only"
            ),
            remediation: Some(
                "Run `orbit mcp callers init` to declare who may call this machine, then raise a \
                 row to operator by hand if one should dispatch work here."
                    .to_string(),
            ),
        }
    } else {
        WorkspaceDoctorResult {
            check_name: "mcp-callers".to_string(),
            status: WorkspaceDoctorStatus::Skipped,
            message: "this machine accepts no SSH logins, so it serves no remote MCP callers"
                .to_string(),
            remediation: None,
        }
    };

    let keys = match (
        health.defect.is_some(),
        health.unpinned_operator_callers.as_slice(),
    ) {
        (true, _) => WorkspaceDoctorResult {
            check_name: "mcp-caller-keys".to_string(),
            status: WorkspaceDoctorStatus::Skipped,
            message: "the callers file does not load, so its grants cannot be inspected"
                .to_string(),
            remediation: None,
        },
        (false, []) if health.present => WorkspaceDoctorResult {
            check_name: "mcp-caller-keys".to_string(),
            status: WorkspaceDoctorStatus::Ok,
            message: "every operator grant is bound to an SSH key".to_string(),
            remediation: None,
        },
        (false, []) => WorkspaceDoctorResult {
            check_name: "mcp-caller-keys".to_string(),
            status: WorkspaceDoctorStatus::Skipped,
            message: "no callers file, so no operator grants to bind".to_string(),
            remediation: None,
        },
        (false, unpinned) => WorkspaceDoctorResult {
            check_name: "mcp-caller-keys".to_string(),
            status: WorkspaceDoctorStatus::Warning,
            message: format!(
                "{file} grants operator to {} with no ssh_key_fingerprint, so the grant rests on \
                 a machine_id the caller asserts rather than a key it holds",
                unpinned.join(", ")
            ),
            remediation: Some(format!(
                "Prepare the protected Linux login-shell launcher, then run `orbit mcp callers \
                 authorize --machine-id {} --key <caller-key>.pub --launcher \
                 <protected-orbit>`, install the printed authorized_keys line, and add the \
                 fingerprint it reports to the row.",
                unpinned.first().map_or("<machine-id>", String::as_str)
            )),
        },
    };
    vec![callers, keys]
}

/// Whether the OS sweep-clock unit invokes this binary [ORB-12244].
fn clock_unit_row() -> WorkspaceDoctorResult {
    match orbit_core::application::routines::inspect_clock_unit() {
        Ok(inspection) => clock_unit_row_from_inspection(&inspection),
        Err(error) => WorkspaceDoctorResult {
            check_name: "clock-unit".to_string(),
            status: WorkspaceDoctorStatus::Warning,
            message: format!("could not inspect the sweep clock unit: {error}"),
            remediation: Some(
                "Fix the home-directory or unit-file error named above, then rerun `orbit doctor`."
                    .to_string(),
            ),
        },
    }
}

pub(crate) fn clock_unit_row_from_inspection(
    inspection: &orbit_core::application::routines::ClockUnitInspection,
) -> WorkspaceDoctorResult {
    use orbit_core::application::routines::ClockUnitVerdict;

    let status = match inspection.verdict {
        ClockUnitVerdict::Matching => WorkspaceDoctorStatus::Ok,
        ClockUnitVerdict::NoUnitInstalled => WorkspaceDoctorStatus::Skipped,
        ClockUnitVerdict::PathMismatch
        | ClockUnitVerdict::InvocationMismatch
        | ClockUnitVerdict::Unrunnable { .. } => WorkspaceDoctorStatus::Warning,
        ClockUnitVerdict::VersionMismatch => WorkspaceDoctorStatus::Error,
    };
    WorkspaceDoctorResult {
        check_name: "clock-unit".to_string(),
        status,
        message: inspection.doctor_message(),
        remediation: inspection.doctor_remediation(),
    }
}

fn status_label(status: WorkspaceDoctorStatus) -> &'static str {
    match status {
        WorkspaceDoctorStatus::Ok => "ok",
        WorkspaceDoctorStatus::Warning => "warning",
        WorkspaceDoctorStatus::Error => "ERROR",
        WorkspaceDoctorStatus::Skipped => "skipped",
    }
}

/// Render `--fix-orphan-task-stores --confirm`'s outcome, naming the empty
/// and populated partition counts separately so a run that deleted task
/// bundles is never described as removing only empty partitions [ORB-12144].
pub(crate) fn orphan_task_store_removal_message(removed: &OrphanTaskStoreRemoval) -> String {
    format!(
        "Removed {} empty orphaned task-store partition(s) and {} populated partition(s) \
         ({} task bundle(s)).",
        removed.empty_partitions, removed.populated_partitions, removed.task_bundles
    )
}

pub(crate) fn human_detail(row: &WorkspaceDoctorResult) -> String {
    row.remediation.as_ref().map_or_else(
        || row.message.clone(),
        |remediation| format!("{}\nAction: {remediation}", row.message),
    )
}

pub(crate) fn doctor_row_json(row: &WorkspaceDoctorResult) -> Value {
    json!({
        "check": row.check_name,
        "status": match row.status {
            WorkspaceDoctorStatus::Ok => "ok",
            WorkspaceDoctorStatus::Warning => "warning",
            WorkspaceDoctorStatus::Error => "error",
            WorkspaceDoctorStatus::Skipped => "skipped",
        },
        "message": row.message,
        "remediation": row.remediation,
    })
}
