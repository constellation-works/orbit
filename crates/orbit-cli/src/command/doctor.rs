use clap::{Args, Subcommand};
use orbit_cmd::{
    DoctorCommands, OrphanTaskStoreRemoval, WorkspaceDoctorResult, WorkspaceDoctorStatus,
};
use orbit_core::{DOCTOR_FINDINGS_MESSAGE_PREFIX, OrbitError, OrbitRuntime};
use orbit_types::policy::{DEFAULT_POLICY_NAME, FsOperation};
use serde_json::{Value, json};

use crate::command::{Block, CommandOut, Execute, Payload};
use crate::output::color::{Domain, Role};

/// `orbit doctor` — workspace-level self-diagnostics [ORB-10005].
#[derive(Args)]
#[command(
    about = "Diagnose workspace health, provider CLIs, and filesystem access",
    long_about = "Diagnose workspace health, provider CLIs, and filesystem access\n\n\
        With no subcommand, checks the workspace: config, database, disk, indexes, locks, and \
        runs. Default database checks read the header and schema; --deep also scans every \
        database page with SQLite quick_check. The subcommands run focused diagnostics for provider CLIs and filesystem access.",
    args_conflicts_with_subcommands = true
)]
pub struct DoctorCommand {
    /// Run a focused diagnostic instead of the workspace health checks.
    #[command(subcommand)]
    pub command: Option<DoctorSubcommand>,

    /// Emit machine-readable JSON instead of the table.
    #[arg(long)]
    pub json: bool,

    /// Scan every database page with SQLite quick_check; default checks only read the header and schema.
    #[arg(long)]
    pub deep: bool,

    /// Clear dead holder records under the lock, preserving lock files, before diagnosing the workspace.
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

    /// Delete this Orbit root and workspace's own state-routine attempt pins that no consumer or live run still names. Legacy shared pins, other roots' and workspaces' pins, in-use pins and unrecognized refs are kept and reported. Refuses while a routine sweep runs.
    #[arg(long)]
    pub fix_automation_pins: bool,

    /// Confirm a destructive repair. Required by --fix-orphan-task-stores, which deletes partition directories and their task bundles.
    #[arg(long)]
    pub confirm: bool,
}

#[derive(Subcommand)]
pub enum DoctorSubcommand {
    /// Show each executor's provider CLI, configured sandbox, and Linux sandbox readiness
    Providers(ProvidersArgs),
    /// Dry-run a workspace-relative path against a filesystem profile's read and modify rules
    FsAccess(FsAccessArgs),
}

#[derive(Args)]
pub struct ProvidersArgs {
    /// Emit machine-readable JSON instead of the table.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct FsAccessArgs {
    /// Filesystem profile name (for example `implementer`)
    pub profile: String,
    /// Path to check, matched as written against the profile's workspace-relative rules
    pub path: String,
    /// Emit machine-readable JSON instead of the detail view.
    #[arg(long)]
    pub json: bool,
}

impl Execute for DoctorCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self.command {
            Some(DoctorSubcommand::Providers(_)) => return provider_diagnostics(runtime),
            Some(DoctorSubcommand::FsAccess(args)) => {
                return fs_access(runtime, &args.profile, &args.path);
            }
            None => {}
        }
        let mut results = Vec::new();
        if self.fix_stale_locks {
            let started = std::time::Instant::now();
            let cleared = runtime.remove_stale_lock_files()?;
            eprintln!("Cleared {cleared} stale lock holder record(s).");
            results.push(WorkspaceDoctorResult {
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                check_name: "fix-stale-locks".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message: format!("Cleared {cleared} stale lock holder record(s)."),
                remediation: None,
            });
        }
        if self.fix_stale_task_locks {
            let started = std::time::Instant::now();
            let released = runtime.clear_stale_task_reservations()?;
            eprintln!("Released {released} stale task reservation(s).");
            results.push(WorkspaceDoctorResult {
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                check_name: "fix-stale-task-locks".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message: format!("Released {released} stale task reservation(s)."),
                remediation: None,
            });
        }
        if self.remove_graph {
            let started = std::time::Instant::now();
            let removed = runtime.remove_retired_graph_state()?;
            eprintln!("Removed {removed} retired graph location(s).");
            results.push(WorkspaceDoctorResult {
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                check_name: "remove-graph".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message: format!("Removed {removed} retired graph location(s)."),
                remediation: None,
            });
        }
        if self.fix_stale_artifacts {
            let started = std::time::Instant::now();
            let removed = runtime.remove_stale_definition_artifacts()?;
            eprintln!("Retired {removed} deprecated definition artifact(s).");
            results.push(WorkspaceDoctorResult {
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                check_name: "fix-stale-artifacts".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message: format!("Retired {removed} deprecated definition artifact(s)."),
                remediation: None,
            });
        }
        if self.fix_retired_activity_backends {
            let started = std::time::Instant::now();
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
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
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
            let started = std::time::Instant::now();
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
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                check_name: "fix-orphan-task-stores".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message,
                remediation: None,
            });
        }
        if self.fix_automation_pins {
            let started = std::time::Instant::now();
            let cleanup =
                orbit_core::application::automation::release_unreferenced_attempt_pins(runtime)?;
            let message = automation_pin_cleanup_message(&cleanup);
            eprintln!("{message}");
            results.push(WorkspaceDoctorResult {
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                check_name: "fix-automation-pins".to_string(),
                status: if cleanup.kept.is_empty()
                    && (cleanup.refused.is_none() || cleanup.retained_unproven == 0)
                {
                    WorkspaceDoctorStatus::Ok
                } else {
                    WorkspaceDoctorStatus::Warning
                },
                message,
                remediation: None,
            });
        }
        results.extend(orbit_cmd::run_doctor_report(runtime, self.deep));
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
        let mut payload = Payload::blocks(Value::Array(values), blocks).with_exit_code(exit_code);
        if failures > 0 {
            payload = payload.with_audit_message(findings_audit_message(&results, warnings));
        }
        Ok(payload.into())
    }
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

/// `orbit doctor fs-access`: the active policy's read and modify verdicts for
/// one path under one fsProfile, without running anything.
pub(crate) fn fs_access(runtime: &OrbitRuntime, profile: &str, path: &str) -> CommandOut {
    let def = runtime
        .get_policy_def(DEFAULT_POLICY_NAME)?
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!("policy not found: {DEFAULT_POLICY_NAME}"))
        })?;
    let read = def.check_path(profile, FsOperation::Read, path)?;
    let modify = def.check_path(profile, FsOperation::Modify, path)?;

    let doc = json!({
        "policy": DEFAULT_POLICY_NAME,
        "profile": profile,
        "path": path,
        "read": {
            "allowed": read.allowed,
            "matched_rule": read.matched_rule,
        },
        "modify": {
            "allowed": modify.allowed,
            "matched_rule": modify.matched_rule,
        },
    });
    let verdict = |allowed: bool| if allowed { "allowed" } else { "denied" };
    let text = format!(
        "Policy:  {DEFAULT_POLICY_NAME}\nProfile: {profile}\nPath:    {path}\nread:    {} ({})\nmodify:  {} ({})",
        verdict(read.allowed),
        read.matched_rule,
        verdict(modify.allowed),
        modify.matched_rule
    );
    Ok(Payload::detail(doc, text).into())
}

/// `orbit doctor providers`: one row per executor definition, naming the
/// provider CLI it launches, where dispatch would find that CLI, and the
/// sandbox mode the effective definition resolves to.
fn provider_diagnostics(runtime: &OrbitRuntime) -> CommandOut {
    use crate::output::table::{Column, Table};

    let defs = runtime.list_executor_defs()?;
    // One fresh user-scoped probe for this diagnostic. The configured executor
    // mode alone does not establish that the host can create the namespace.
    let linux_probe = defs
        .iter()
        .any(|def| {
            def.sandbox
                .is_some_and(|kind| kind.as_str() == "linux-bwrap")
        })
        .then(orbit_core::bootstrap::linux_sandbox_host::probe_bwrap_fresh);
    let mut values = Vec::with_capacity(defs.len());
    let mut table = Table::new(vec![
        Column::new("EXECUTOR").fixed(),
        Column::new("TYPE").fixed(),
        Column::new("CLI").fixed(),
        Column::new("FOUND").fixed(),
        Column::new("SANDBOX").fixed(),
        Column::new("READY").fixed(),
        Column::new("BWRAP").fixed(),
        Column::new("LAUNCHER").path(),
    ])
    .empty_message("no executors defined");
    for def in &defs {
        let launcher = def
            .command
            .as_deref()
            .and_then(|program| runtime.locate_provider_launcher(program));
        // An executor without a `command` (e.g. `local-shell`) launches no
        // provider CLI, so availability does not apply rather than failing.
        let cli_available = def.command.as_ref().map(|_| launcher.is_some());
        let sandbox = def.sandbox.map_or("unspecified", |kind| kind.as_str());
        let readiness = (sandbox == "linux-bwrap")
            .then_some(linux_probe.as_ref())
            .flatten();
        // Which trusted Bubblewrap the probe selected — the host's or the
        // one bundled with Orbit — and its version once it passed.
        let wrapper = readiness.and_then(|probe| probe.source.map(|source| (source, probe)));
        values.push(json!({
            "name": def.name,
            "executor_type": def.executor_type.to_string(),
            "command": def.command,
            "args": def.args,
            "cli_available": cli_available,
            "launcher": launcher.as_ref().map(|path| path.display().to_string()),
            "sandbox": def.sandbox,
            "sandbox_ready": readiness.map(|probe| probe.available),
            "sandbox_readiness_detail": readiness.map(|probe| probe.detail.as_str()),
            "sandbox_wrapper": wrapper.map(|(source, _)| source.as_str()),
            "sandbox_wrapper_path": wrapper.map(|(_, probe)| probe.trusted_path.as_str()),
            "sandbox_wrapper_version": wrapper.and_then(|(_, probe)| probe.version.as_deref()),
            "allow_fallback": def.allow_fallback,
        }));
        table.add_row(vec![
            def.name.clone(),
            def.executor_type.to_string(),
            def.command.clone().unwrap_or_else(|| "-".to_string()),
            match cli_available {
                Some(true) => "yes",
                Some(false) => "no",
                None => "-",
            }
            .to_string(),
            sandbox.to_string(),
            readiness.map_or_else(
                || "-".to_string(),
                |probe| if probe.available { "yes" } else { "no" }.to_string(),
            ),
            wrapper.map_or_else(
                || "-".to_string(),
                |(source, probe)| match &probe.version {
                    Some(version) => format!("{} {version}", source.as_str()),
                    None => source.as_str().to_string(),
                },
            ),
            launcher.map_or_else(|| "-".to_string(), |path| path.display().to_string()),
        ]);
    }
    Ok(Payload::list(values, table).into())
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
fn orphan_task_store_removal_message(removed: &OrphanTaskStoreRemoval) -> String {
    format!(
        "Removed {} empty orphaned task-store partition(s) and {} populated partition(s) \
         ({} task bundle(s)).",
        removed.empty_partitions, removed.populated_partitions, removed.task_bundles
    )
}

/// Render `--fix-automation-pins`'s outcome: what it reclaimed, why the rest
/// of its own namespace stayed, and every pin it retained without proof of
/// ownership.
fn automation_pin_cleanup_message(
    cleanup: &orbit_core::application::automation::AttemptPinCleanup,
) -> String {
    let mut message = match &cleanup.refused {
        Some(reason) => format!(
            "Reclaimed no automation attempt pins: ownership of {} is not proved ({reason}); \
             retained {} owned pin(s).",
            cleanup.namespace, cleanup.retained_unproven,
        ),
        None => format!(
            "Reclaimed {} unreferenced automation attempt pin(s) under {}; kept {} in flight, \
             {} live-run, {} assessed.",
            cleanup.released.len(),
            cleanup.namespace,
            cleanup.retained_active,
            cleanup.retained_live_run,
            cleanup.retained_assessed,
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
