use orbit_common::Env;
use orbit_common::OrbitError;
use orbit_core::Core;
use orbit_core::app::doctor::{
    AttemptPinCleanup, Command, OrphanTaskStoreRemoval, Outcome, Repaired, Repairs,
    RetiredConfigKeyRepair, WorkspaceDoctorResult, WorkspaceDoctorStatus, doctor_row_json, exec,
    misplaced_plugin_enablement_row,
};
use orbit_types::telemetry::incident::DOCTOR_FINDINGS_MESSAGE_PREFIX;
use serde_json::Value;

use super::command::DoctorCommand;
use super::support::status_label;
use crate::command::{Block, CommandOut, Payload};
use crate::output::common::color::{Domain, Role};

impl DoctorCommand {
    /// The report `orbit doctor` can still give when the runtime would not
    /// open, or `None` to let the bootstrap error stand.
    ///
    /// Only a plain health check qualifies: a focused subcommand or a repair
    /// flag needs the runtime it asked for. The plain check opens the
    /// runtime write-free, so a workspace whose stores were never created or
    /// whose stores need a migration fails that open; the report then names
    /// the setup step instead of the bare storage error (STD-01 §R31). Stores
    /// that exist but whose open refusal says a writable open is still owed
    /// are the migration case. A store this binary cannot read (an
    /// unsupported schema, which a writable open cannot migrate) and any other
    /// bootstrap failure with both stores present still fail closed with the
    /// store's own upgrade or recovery guidance. A misplaced
    /// global `[plugin_enablement]` table, read straight from the host config
    /// because that table is what makes the layered config refuse to load, is
    /// reported alongside.
    pub(crate) fn degraded_report(
        &self,
        root_override: Option<&std::path::Path>,
        bootstrap: &OrbitError,
    ) -> Option<CommandOut> {
        if self.command.is_some() || self.repairs.any_requested() {
            return None;
        }
        let cwd = std::env::current_dir().ok()?;
        let global_root =
            orbit_cmd::registry_runtime::RegisteredRuntimeFactory::resolve_roots_for_cwd(
                &cwd,
                root_override,
            )
            .ok()?
            .global_root;
        let misplaced = misplaced_plugin_enablement_row(&global_root);
        let missing_store = !global_root.join("orbit.db").exists()
            || !global_root.join("tasks").join("index.sqlite").exists();
        let needs_migration = !missing_store && bootstrap.owes_writable_store_open();
        if misplaced.is_none() && !missing_store && !needs_migration {
            return None;
        }
        let remediation = if missing_store {
            "Run `orbit task list` once to create the workspace stores, then rerun `orbit doctor`."
        } else if needs_migration {
            "Run `orbit task list` once to migrate the workspace stores, then rerun `orbit doctor`."
        } else {
            "Fix the check above, then rerun `orbit doctor` for the full report."
        };
        let runtime = WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "runtime".to_string(),
            status: WorkspaceDoctorStatus::Error,
            message: format!("The runtime did not open, so no other check ran: {bootstrap}"),
            remediation: Some(remediation.to_string()),
        };
        Some(report_output(
            misplaced.into_iter().chain([runtime]).collect(),
            self.problems,
            self.check.as_deref(),
        ))
    }
}

impl DoctorCommand {
    /// The bare `orbit doctor` body: optional repairs, then the workspace report.
    pub(super) fn workspace_checks(self, core: &Core, env: &Env) -> CommandOut {
        let flags = &self.repairs;
        let repairs = Repairs {
            missing_pr_refs: flags.fix_missing_pr_refs,
            state_directory_permissions: flags.fix_state_directory_permissions,
            stale_locks: flags.fix_stale_locks,
            stale_task_locks: flags.fix_stale_task_locks,
            graph: flags.remove_graph,
            stale_artifacts: flags.fix_stale_artifacts,
            orphan_task_stores: flags.fix_orphan_task_stores,
            automation_pins: flags.fix_automation_pins,
            retired_config_keys: flags.fix_retired_config_keys,
        };
        let mut repaired_rows = Vec::new();
        let mut repaired = |repair: Repaired, started: std::time::Instant| {
            push_repaired(&mut repaired_rows, repair, started);
        };
        let Outcome::Report { cpu_source, rows } = exec(
            core,
            Command::Report {
                repairs,
                confirm: self.confirm,
                deep: self.deep,
                env: env.clone(),
                host: orbit_cmd::doctor_host(),
                repaired: &mut repaired,
            },
        )?
        else {
            unreachable!("a doctor report command answers with its rows")
        };
        let mut results = vec![WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "resource-throttle".to_string(),
            status: WorkspaceDoctorStatus::Info,
            message: format!("CPU source: {cpu_source}"),
            remediation: None,
        }];
        results.extend(repaired_rows);
        results.extend(rows);
        report_output(results, self.problems, self.check.as_deref())
    }
}

/// Report one finished repair on stderr and add its row.
fn push_repaired(
    results: &mut Vec<WorkspaceDoctorResult>,
    repair: Repaired,
    started: std::time::Instant,
) {
    let (check_name, status, message) = match repair {
        Repaired::MissingPrRefs(repaired) => (
            "fix-missing-pr-refs",
            WorkspaceDoctorStatus::Ok,
            format!(
                "Restored pull request references on {} review tasks.",
                repaired.len()
            ),
        ),
        Repaired::StateDirectoryPermissions(repaired) => (
            "fix-state-directory-permissions",
            WorkspaceDoctorStatus::Ok,
            format!(
                "Restricted {repaired} writable Orbit state director{} to owner-only access.",
                if repaired == 1 { "y" } else { "ies" }
            ),
        ),
        Repaired::StaleLocks(cleared) => (
            "fix-stale-locks",
            WorkspaceDoctorStatus::Ok,
            format!("Cleared {cleared} stale lock holder record(s)."),
        ),
        Repaired::StaleTaskLocks(released) => (
            "fix-stale-task-locks",
            WorkspaceDoctorStatus::Ok,
            format!("Released {released} stale task reservation(s)."),
        ),
        Repaired::Graph(removed) => (
            "remove-graph",
            WorkspaceDoctorStatus::Ok,
            format!("Removed {removed} retired graph location(s)."),
        ),
        Repaired::StaleArtifacts(removed) => (
            "fix-stale-artifacts",
            WorkspaceDoctorStatus::Ok,
            format!("Retired {removed} deprecated definition artifact(s)."),
        ),
        Repaired::OrphanTaskStores(removed) => (
            "fix-orphan-task-stores",
            WorkspaceDoctorStatus::Ok,
            orphan_task_store_removal_message(&removed),
        ),
        Repaired::AutomationPins(cleanup) => (
            "fix-automation-pins",
            if cleanup.kept.is_empty()
                && (cleanup.refused.is_none() || cleanup.retained_unproven == 0)
            {
                WorkspaceDoctorStatus::Ok
            } else {
                WorkspaceDoctorStatus::Warning
            },
            automation_pin_cleanup_message(&cleanup),
        ),
        Repaired::RetiredConfigKeys(repair) => (
            "fix-retired-config-keys",
            WorkspaceDoctorStatus::Ok,
            retired_config_key_repair_message(&repair),
        ),
    };
    push_repair_result(results, check_name, started, status, message);
}

fn push_repair_result(
    results: &mut Vec<WorkspaceDoctorResult>,
    check_name: &'static str,
    started: std::time::Instant,
    status: WorkspaceDoctorStatus,
    message: String,
) {
    eprintln!("{message}");
    results.push(WorkspaceDoctorResult {
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        check_name: check_name.to_string(),
        status,
        message,
        remediation: None,
    });
}

/// Render the checks as `orbit doctor` reports them: a table sorted by
/// severity, the JSON rows, and a nonzero exit when any check failed.
fn report_output(
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
            doctor_row_json(row),
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

    let values = results.iter().map(doctor_row_json).collect::<Vec<_>>();

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
        use crate::output::common::table::{Column, Table};
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
                crate::output::common::color::cell(status_label(row.status), Domain::DoctorStatus),
                Cell::new(human_detail(row).replace('\n', " | ")),
            ]);
        }
        blocks.push(Block::table(table));
        blocks.push(Block::DoctorFindings(results.clone()));

        if failures == 0 && warnings == 0 {
            blocks.push(Block::notice(format!(
                "\n{}",
                crate::output::common::color::text("Workspace healthy.", Role::Ok)
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

/// Render `--fix-orphan-task-stores --confirm`'s outcome, naming the empty
/// and populated partition counts separately so a run that deleted task
/// bundles is never described as removing only empty partitions [ORB-12144].
fn orphan_task_store_removal_message(removed: &OrphanTaskStoreRemoval) -> String {
    format!(
        "Removed {} empty orphaned task-store partition(s) and {} populated partition(s) \
         ({} task bundle(s)).",
        removed.empty_partitions, removed.populated_partitions, removed.task_bundles
    )
}

/// Render `--fix-retired-config-keys`: which replacement keys were written,
/// which keys were removed, and the backup written beside each edited file.
/// An empty repair changed nothing.
fn retired_config_key_repair_message(repair: &RetiredConfigKeyRepair) -> String {
    if repair.removed.is_empty() && repair.written.is_empty() {
        return "No retired, deprecated or unknown config keys were present.".to_string();
    }
    let mut groups: Vec<(String, String, Vec<&str>)> = Vec::new();
    for entry in &repair.removed {
        let config = entry.config_path.display().to_string();
        let backup = entry.backup_path.display().to_string();
        if let Some(group) = groups.iter_mut().find(|group| group.0 == config) {
            group.2.push(entry.key.as_str());
        } else {
            groups.push((config, backup, vec![entry.key.as_str()]));
        }
    }
    let detail = groups
        .iter()
        .map(|(config, backup, keys)| {
            format!("{} from {config} (backup {backup})", keys.join(", "))
        })
        .collect::<Vec<_>>()
        .join("; ");
    let count = repair.removed.len();
    let removed = format!(
        "Removed {count} retired, deprecated or unknown config key{}: {detail}",
        if count == 1 { "" } else { "s" }
    );
    let written = repair
        .written
        .iter()
        .map(|entry| format!("{} in {}", entry.key, entry.config_path.display()))
        .collect::<Vec<_>>()
        .join(", ");
    match (count, written.is_empty()) {
        (_, true) => format!("{removed}."),
        (0, false) => format!("Rewrote deprecated config values: {written}."),
        (_, false) => format!("{removed}; wrote their replacements first: {written}."),
    }
}

/// Render `--fix-automation-pins`'s outcome: what it reclaimed, why the rest
/// of its own namespace stayed, and every pin it retained without proof of
/// ownership.
fn automation_pin_cleanup_message(cleanup: &AttemptPinCleanup) -> String {
    let mut message = match &cleanup.refused {
        Some(reason) => format!(
            "Reclaimed no automation attempt pins: ownership of {} is not proved ({reason}); \
             retained {} owned pin(s).",
            cleanup.namespace, cleanup.retained_unproven,
        ),
        None => format!(
            "Reclaimed {} unreferenced automation attempt pin(s) under {}; kept {} in flight, \
             {} live-run.",
            cleanup.released.len(),
            cleanup.namespace,
            cleanup.retained_active,
            cleanup.retained_live_run,
        ),
    };
    message.push_str(&format!(
        " Retained {} legacy shared pin(s) under refs/orbit/automation/ with no recorded owner \
         and {} pin namespace(s) owned by other Orbit roots or workspaces.",
        cleanup.retained_legacy, cleanup.foreign_owners,
    ));
    if !cleanup.kept.is_empty() {
        message.push_str(&format!(
            " Could not delete (moved or locked): {}.",
            cleanup.kept.join(", ")
        ));
    }
    if !cleanup.unrecognized.is_empty() {
        message.push_str(&format!(
            " Left unrecognized refs untouched: {}.",
            cleanup.unrecognized.join(", ")
        ));
    }
    message
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
