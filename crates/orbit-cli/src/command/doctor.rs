use clap::{Args, Subcommand};
use orbit_cmd::{
    DoctorCommands, OrphanTaskStoreRemoval, WorkspaceDoctorResult, WorkspaceDoctorStatus,
};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_types::policy::{DEFAULT_POLICY_NAME, FsOperation};
use serde_json::{Value, json};

use crate::command::{Block, CommandOut, Execute, Payload};
use crate::output::color::{Domain, Role};

/// `orbit doctor` — workspace-level self-diagnostics [ORB-10005].
#[derive(Args)]
#[command(
    about = "Diagnose workspace health (config, database, disk, indexes, locks, runs)",
    args_conflicts_with_subcommands = true
)]
pub struct DoctorCommand {
    /// Run a focused diagnostic instead of the workspace health checks.
    #[command(subcommand)]
    pub command: Option<DoctorSubcommand>,

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

#[derive(Subcommand)]
pub enum DoctorSubcommand {
    /// Show each executor's provider CLI, whether dispatch can find it, and its sandbox mode
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
        results.push(state_directory_permissions_row(runtime));
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
pub(crate) fn provider_diagnostics(runtime: &OrbitRuntime) -> CommandOut {
    use crate::output::table::{Column, Table};

    let defs = runtime.list_executor_defs()?;
    let mut values = Vec::with_capacity(defs.len());
    let mut table = Table::new(vec![
        Column::new("EXECUTOR").fixed(),
        Column::new("TYPE").fixed(),
        Column::new("CLI").fixed(),
        Column::new("FOUND").fixed(),
        Column::new("SANDBOX").fixed(),
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
        values.push(json!({
            "name": def.name,
            "executor_type": def.executor_type.to_string(),
            "command": def.command,
            "args": def.args,
            "cli_available": cli_available,
            "launcher": launcher.as_ref().map(|path| path.display().to_string()),
            "sandbox": def.sandbox,
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
            launcher.map_or_else(|| "-".to_string(), |path| path.display().to_string()),
        ]);
    }
    Ok(Payload::list(values, table).into())
}

/// Report Orbit-owned state directories whose write bits let another local
/// principal replace or unlink private files held beneath them.
pub(crate) fn state_directory_permissions_row(runtime: &OrbitRuntime) -> WorkspaceDoctorResult {
    #[cfg(unix)]
    {
        use std::collections::BTreeSet;
        use std::os::unix::fs::PermissionsExt;

        fn visit(
            path: &std::path::Path,
            descend: bool,
            seen: &mut BTreeSet<std::path::PathBuf>,
            writable: &mut Vec<(std::path::PathBuf, u32)>,
        ) -> std::io::Result<()> {
            if !seen.insert(path.to_path_buf()) {
                return Ok(());
            }
            let metadata = std::fs::symlink_metadata(path)?;
            if !metadata.is_dir() {
                return Ok(());
            }
            let mode = metadata.permissions().mode() & 0o777;
            if mode & 0o022 != 0 {
                writable.push((path.to_path_buf(), mode));
            }
            if !descend {
                return Ok(());
            }
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    visit(&entry.path(), true, seen, writable)?;
                }
            }
            Ok(())
        }

        let mut seen = BTreeSet::new();
        let mut writable = Vec::new();
        let global = runtime.global_root();
        let workspace = runtime.paths().orbit_dir.clone();
        let configured_roots = [
            (global.clone(), false),
            (global.join("state"), true),
            (global.join("tasks"), true),
            (global.join("cache"), true),
            (global.join("frictions"), true),
            (workspace.clone(), false),
            (workspace.join("state"), true),
            (workspace.join("tasks"), true),
            (workspace.join("frictions"), true),
        ];
        for (configured_root, descend) in configured_roots {
            let root = match configured_root.canonicalize() {
                Ok(root) => root,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return WorkspaceDoctorResult {
                        check_name: "state-directory-permissions".to_string(),
                        status: WorkspaceDoctorStatus::Error,
                        message: format!(
                            "could not resolve Orbit state directory '{}': {error}",
                            configured_root.display()
                        ),
                        remediation: Some(
                            "Fix the directory access error named above, then rerun `orbit doctor`."
                                .to_string(),
                        ),
                    };
                }
            };
            if let Err(error) = visit(&root, descend, &mut seen, &mut writable) {
                return WorkspaceDoctorResult {
                    check_name: "state-directory-permissions".to_string(),
                    status: WorkspaceDoctorStatus::Error,
                    message: format!(
                        "could not inspect Orbit state directory '{}': {error}",
                        root.display()
                    ),
                    remediation: Some(
                        "Fix the directory access error named above, then rerun `orbit doctor`."
                            .to_string(),
                    ),
                };
            }
        }

        if writable.is_empty() {
            return WorkspaceDoctorResult {
                check_name: "state-directory-permissions".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message: "all Orbit state directories deny group/world write access".to_string(),
                remediation: None,
            };
        }

        let sample = writable
            .iter()
            .take(5)
            .map(|(path, mode)| format!("{} ({mode:04o})", path.display()))
            .collect::<Vec<_>>()
            .join(", ");
        let remainder = writable.len().saturating_sub(5);
        let suffix = if remainder == 0 {
            String::new()
        } else {
            format!(", and {remainder} more")
        };
        WorkspaceDoctorResult {
            check_name: "state-directory-permissions".to_string(),
            status: WorkspaceDoctorStatus::Warning,
            message: format!(
                "{} Orbit state director{} group/world writable: {sample}{suffix}",
                writable.len(),
                if writable.len() == 1 {
                    "y is"
                } else {
                    "ies are"
                }
            ),
            remediation: Some(
                "Remove group/world write permission from every named directory (for example, \
                 `chmod go-w <directory>`), then rerun `orbit doctor`."
                    .to_string(),
            ),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = runtime;
        WorkspaceDoctorResult {
            check_name: "state-directory-permissions".to_string(),
            status: WorkspaceDoctorStatus::Skipped,
            message: "Unix directory mode checks are not available on this platform".to_string(),
            remediation: None,
        }
    }
}

/// Whether this machine still carries retired destination-side MCP caller
/// authorization files [ORB-12564].
///
/// One row, and never an error. The files grant and refuse nothing now: an SSH
/// login to this machine is ownership of it, so a session served over SSH holds
/// the authority its argv asks for, exactly as a local one does. What is worth
/// saying is that a ceiling an operator wrote is inert, because the operator
/// who wrote it has no other way to find out.
fn caller_authorization_rows() -> Vec<WorkspaceDoctorResult> {
    let Ok(home) = orbit_common::fs::path::home_dir() else {
        return Vec::new();
    };
    let ignored = orbit_mcp::ignored_caller_authorization_paths(&home.join(".orbit"));
    if ignored.is_empty() {
        return vec![WorkspaceDoctorResult {
            check_name: "mcp-callers".to_string(),
            status: WorkspaceDoctorStatus::Ok,
            message: "no retired MCP caller-authorization files; a session served over SSH \
                      holds the authority its argv asks for"
                .to_string(),
            remediation: None,
        }];
    }
    let paths = ignored
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>();
    vec![WorkspaceDoctorResult {
        check_name: "mcp-callers".to_string(),
        status: WorkspaceDoctorStatus::Warning,
        message: format!(
            "left over from destination-side caller authorization and ignored, capping no \
             remote MCP session: {}",
            paths.join(", ")
        ),
        remediation: Some(format!(
            "Delete it: `rm -r {}`. To deny a caller, remove its key from \
             `~/.ssh/authorized_keys` — that is the only boundary this machine ever had.",
            paths.join(" ")
        )),
    }]
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
