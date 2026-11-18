use clap::{Args, Subcommand};
use orbit_core::OrbitRuntime;

use super::admission_receipts::{self, AdmissionReceiptsArgs};
use super::fs_access::{self, FsAccessArgs};
use super::providers::{self, ProvidersArgs};
use crate::command::{CommandOut, Execute};

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
    #[arg(long, value_name = "NAME", conflicts_with_all = ["problems", "doctor_repairs", "confirm"])]
    pub check: Option<String>,

    #[command(flatten)]
    pub repairs: DoctorRepairFlags,

    /// Confirm a destructive repair. Required by --fix-orphan-task-stores, which deletes partition directories and their task bundles.
    #[arg(long)]
    pub confirm: bool,
}

#[derive(Args, Default, PartialEq, Eq)]
#[group(id = "doctor_repairs", multiple = true)]
pub struct DoctorRepairFlags {
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
}

impl DoctorRepairFlags {
    pub(crate) fn any_requested(&self) -> bool {
        self != &Self::default()
    }
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
        self.workspace_checks(runtime)
    }
}
