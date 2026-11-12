use clap::{Args, Subcommand};
use orbit_cmd::{DoctorCommands, WorkspaceDoctorResult, WorkspaceDoctorStatus};
use orbit_core::{OrbitError, OrbitRuntime};

use crate::command::{CommandOut, Execute};

use super::admission_receipts;
use super::fs_access;
use super::providers;
use super::report;
use super::support;

/// `orbit doctor` — workspace-level self-diagnostics [ORB-10005].
#[derive(Args)]
#[command(
    about = "Diagnose workspace health, provider CLIs, and filesystem access",
    long_about = "Diagnose workspace health, provider CLIs, and filesystem access\n\n\
        With no subcommand, checks the workspace: config, database, disk, indexes, locks, and \
        runs. Default database checks read the header and schema; --deep also scans every \
        database page with SQLite quick_check. The subcommands run focused diagnostics for provider CLIs, filesystem access, and admission receipt storage.",
    args_conflicts_with_subcommands = true
)]
pub struct DoctorCommand {
    /// Run a focused diagnostic instead of the workspace health checks.
    #[command(subcommand)]
    pub command: Option<DoctorSubcommand>,

    /// Scan every database page with SQLite quick_check; default checks only read the header and schema.
    #[arg(long)]
    pub deep: bool,

    /// Print only failures and warnings.
    #[arg(long)]
    pub problems: bool,

    /// Print one check with its complete message and remediation (exact CHECK name)
    #[arg(long, value_name = "NAME", conflicts_with_all = [
        "problems", "fix_stale_locks", "fix_stale_task_locks",
        "fix_state_directory_permissions", "remove_graph", "fix_stale_artifacts",
        "fix_retired_activity_backends", "fix_orphan_task_stores",
        "fix_automation_pins", "fix_retired_config_keys", "fix_missing_pr_refs", "confirm"
    ])]
    pub check: Option<String>,

    /// Clear dead holder records under the lock, preserving lock files, before diagnosing the workspace.
    #[arg(long)]
    pub fix_stale_locks: bool,

    /// Release only task reservations whose owner/task state is conclusively inactive.
    #[arg(long)]
    pub fix_stale_task_locks: bool,

    /// Restrict writable Orbit state directories to owner-only access, excluding run worktrees, Cargo target trees, and child symlinks (Unix only).
    #[arg(long)]
    pub fix_state_directory_permissions: bool,

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

    /// Back up config.toml and remove retired, deprecated or unknown keys, preserving translated review settings. Refuses legacy after-landing policies until manually migrated.
    #[arg(long)]
    pub fix_retired_config_keys: bool,

    /// Restore missing review-task pull request references from the owner's accepted follower handoffs, preserving claim and landing state.
    #[arg(long)]
    pub fix_missing_pr_refs: bool,

    /// Confirm a destructive repair. Required by --fix-orphan-task-stores, which deletes partition directories and their task bundles.
    #[arg(long)]
    pub confirm: bool,
}

#[derive(Subcommand)]
pub enum DoctorSubcommand {
    /// Show admission receipt counts and logical payload bytes, with separate claim-mutation receipt usage
    AdmissionReceipts(AdmissionReceiptsArgs),
    /// Show each executor's provider CLI, configured sandbox, Linux sandbox readiness, and usage limits
    Providers(ProvidersArgs),
    /// Dry-run a workspace-relative path against a filesystem profile's read and modify rules
    FsAccess(FsAccessArgs),
}

#[derive(Args)]
pub struct ProvidersArgs {}

#[derive(Args)]
pub struct AdmissionReceiptsArgs {
    /// Compact idle and settled admission receipts to permanent tombstones; retain full receipts for unsettled claims and failed or revoked claims with a preserved PR.
    #[arg(long)]
    pub compact: bool,
}

#[derive(Args)]
pub struct FsAccessArgs {
    /// Filesystem profile name (for example `implementer`)
    pub profile: String,
    /// Path to check, matched as written against the profile's workspace-relative rules
    pub path: String,
}

impl DoctorCommand {
    /// The report `orbit doctor` can still give when the runtime would not
    /// open, or `None` to let the bootstrap error stand.
    ///
    /// Only a plain health check qualifies: a focused subcommand or a repair
    /// flag needs the runtime it asked for. The one finding reported without
    /// a runtime is a misplaced global `[plugin_enablement]` table, read
    /// straight from the host config because that table is what makes the
    /// layered config refuse to load. Every other command still fails closed.
    pub(crate) fn degraded_report(
        &self,
        root_override: Option<&std::path::Path>,
        bootstrap: &OrbitError,
    ) -> Option<CommandOut> {
        let repairs = self.fix_stale_locks
            || self.fix_stale_task_locks
            || self.fix_state_directory_permissions
            || self.remove_graph
            || self.fix_stale_artifacts
            || self.fix_retired_activity_backends
            || self.fix_orphan_task_stores
            || self.fix_automation_pins
            || self.fix_retired_config_keys
            || self.fix_missing_pr_refs;
        if self.command.is_some() || repairs {
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
        let misplaced = orbit_cmd::misplaced_plugin_enablement_row(&global_root)?;
        let runtime = WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "runtime".to_string(),
            status: WorkspaceDoctorStatus::Error,
            message: format!("The runtime did not open, so no other check ran: {bootstrap}"),
            remediation: Some(
                "Fix the check above, then rerun `orbit doctor` for the full report.".to_string(),
            ),
        };
        Some(report::report_output(
            vec![misplaced, runtime],
            self.problems,
            self.check.as_deref(),
        ))
    }
}

impl Execute for DoctorCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self.command {
            Some(DoctorSubcommand::AdmissionReceipts(args)) => {
                return admission_receipts::admission_receipts(runtime, args.compact);
            }
            Some(DoctorSubcommand::Providers(_)) => {
                return providers::provider_diagnostics(runtime);
            }
            Some(DoctorSubcommand::FsAccess(args)) => {
                return fs_access::fs_access(runtime, &args.profile, &args.path);
            }
            None => {}
        }
        if self.fix_orphan_task_stores && !self.confirm {
            return Err(orbit_core::OrbitError::InvalidInput(
                "--fix-orphan-task-stores deletes task-store partition directories and their task bundles. Pass --confirm to proceed."
                    .to_string(),
            ));
        }
        let mut results = vec![WorkspaceDoctorResult {
            duration_ms: 0,
            check_name: "resource-throttle".to_string(),
            status: WorkspaceDoctorStatus::Info,
            message: format!(
                "CPU source: {}",
                runtime.resource_throttle_settings().cpu_source
            ),
            remediation: None,
        }];
        if self.fix_missing_pr_refs {
            let started = std::time::Instant::now();
            let repaired = runtime.repair_missing_handoff_pr_refs()?;
            results.push(WorkspaceDoctorResult {
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                check_name: "fix-missing-pr-refs".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message: format!(
                    "Restored pull request references on {} review tasks.",
                    repaired.len()
                ),
                remediation: None,
            });
        }
        if self.fix_state_directory_permissions {
            let started = std::time::Instant::now();
            let repaired = runtime.repair_state_directory_permissions()?;
            let message = format!(
                "Restricted {repaired} writable Orbit state director{} to owner-only access.",
                if repaired == 1 { "y" } else { "ies" }
            );
            eprintln!("{message}");
            results.push(WorkspaceDoctorResult {
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                check_name: "fix-state-directory-permissions".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message,
                remediation: None,
            });
        }
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
            let removed = runtime.remove_orphan_task_stores()?;
            let message = support::orphan_task_store_removal_message(&removed);
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
            let message = support::automation_pin_cleanup_message(&cleanup);
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
        if self.fix_retired_config_keys {
            let started = std::time::Instant::now();
            let repair = runtime.remove_retired_config_keys()?;
            let message = support::retired_config_key_repair_message(&repair);
            eprintln!("{message}");
            results.push(WorkspaceDoctorResult {
                duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                check_name: "fix-retired-config-keys".to_string(),
                status: WorkspaceDoctorStatus::Ok,
                message,
                remediation: None,
            });
        }
        results.extend(orbit_cmd::run_doctor_report(runtime, self.deep));
        report::report_output(results, self.problems, self.check.as_deref())
    }
}
