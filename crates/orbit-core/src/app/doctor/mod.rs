//! The doctor app: workspace-level self-diagnostics behind `orbit doctor`
//! [ORB-10005], and its focused subcommands.
//!
//! The doctor noun is not in the operation registry, so [`Command`] is
//! written by hand, one variant per command-line form. Each runs through the
//! chokepoint in `app/exec.rs`, which writes the one invocation row the
//! command line recorded for it before.
//!
//! Complements the narrower `orbit skill doctor` / `orbit tool doctor`
//! surfaces with whole-workspace checks: config validity, store database
//! integrity and schema-ledger version, free disk space on the volume
//! holding `.orbit`, search-index coverage, leftover lock
//! files from crashed holders, orphaned `running`/`pending` job runs, task
//! reservations whose owner or terminal task association is conclusively
//! inactive, task
//! relation/dependency targets that no longer resolve in the registry
//! (grandfathered relations that block index rebuilds — ORB-10305), and
//! unpublished `ORB-*` stub directories that never received `task.yaml`
//! and hold no bundle content (aborted creates that used to fail
//! `orbit task reindex` closed), plus data-bearing `ORB-*` directories
//! missing `task.yaml` (retained unresolved task data, not stubs).
//!
//! Every check degrades rather than errors: subsystems that are absent in a
//! fresh workspace report [`WorkspaceDoctorStatus::Skipped`], and probe
//! failures become `Warning`/`Error` rows instead of aborting the whole
//! diagnosis. The cheap probes shared with the dashboard's
//! `/healthz?detailed=true` ([`DoctorCommands::health_check_store_writable`])
//! also live here.
//!
//! The full read-only report — the workspace checks plus the host rows
//! (state-directory permissions, providers, MCP registration, clock unit,
//! hosts) — is one ordered table of [`DoctorProbe`]s
//! ([`doctor_report_probes`]). `orbit doctor` runs it after any requested
//! repair, and the dashboard's Health › Doctor panel runs each probe under
//! its own time bound, so both surfaces report the same checks with the same
//! fields ([`doctor_row_json`]). Repairs stay on [`DoctorCommands`] and are
//! never part of the report.

mod activity_catalog;

mod artifact;

mod artifact_diagnostic;

mod automation;

mod browser_kit;

mod command;

mod git_protection;

mod host;

mod host_workspace;

mod permission;

mod report;

mod retired_config;

mod run;

mod scan_worker;

mod script_grant;

mod seeding;

mod standing_drain;

mod system;

mod task;

mod worker_containment;

mod worker_token;

mod workspace;

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fs2::FileExt;
use orbit_common::{Env, OrbitError};
use orbit_store::contract::AdmissionReceiptStorage;
use orbit_store::maintenance::migration::SUPPORTED_SCHEMA_VERSION;
use orbit_types::policy::FsCheckResult;
use orbit_types::task::{TASK_ENVELOPE_FILE_NAME, is_valid_orb_task_id};
use orbit_types::workflow::ExecutorDef;
use orbit_types::workspace::WorkspacePaths;
use serde::Serialize;

use super::exec::{self, App, Operation};
use crate::Core;
use crate::OrbitRuntime;
use crate::bootstrap::linux_sandbox_host::BwrapProbeOutcome;

pub use crate::application::automation::AttemptPinCleanup;
pub use artifact::{
    ArtifactCondition, ArtifactFinding, ArtifactHealth, ArtifactKind, ArtifactProvenance,
    inspect_definition_artifacts, remove_stale_definition_artifacts,
};
use automation::*;
use command::*;
pub use command::{
    DoctorCommands, DoctorProbe, OrphanTaskStoreRemoval, WorkspaceDoctorResult,
    WorkspaceDoctorStatus, kernel_workspace_doctor,
};
pub use host::{
    BrowserKit, Host, KitProblem, TaskStorePartition, TaskStorePartitions,
    UnreachableTaskStorePartition,
};
pub use report::{
    doctor_report_probes, doctor_row_json, misplaced_plugin_enablement_row, run_doctor_report,
};
pub use retired_config::RetiredConfigKeyRepair;
#[cfg(feature = "test-support")]
pub use scan_worker::DoctorWorkerSpawnRefusal;
pub(crate) use system::{collect_lock_files, disk_space_check, process_is_alive};
use task::*;
use workspace::*;

/// What the doctor app can be asked to do.
pub enum Command<'a> {
    /// `orbit doctor`: run the requested repairs in order, then every
    /// read-only check.
    Report {
        /// The repairs to run before the checks.
        repairs: Repairs,
        /// Whether the operator confirmed a destructive repair.
        confirm: bool,
        /// Scan every database page with SQLite quick_check.
        deep: bool,
        /// The caller's environment, which the checks judge.
        env: Env,
        /// The host reads above this crate.
        host: Arc<dyn Host>,
        /// Called as each repair finishes, with when it started.
        repaired: &'a mut dyn FnMut(Repaired, std::time::Instant),
    },
    /// `orbit doctor admission-receipts`: the owner's admission receipt
    /// storage, compacted first when asked.
    AdmissionReceipts {
        /// Compact idle and settled receipts to tombstones first.
        compact: bool,
    },
    /// `orbit doctor providers`: each executor's provider CLI and sandbox.
    Providers,
    /// `orbit doctor fs-access`: the default policy's read and modify
    /// verdicts for one path under one filesystem profile.
    FsAccess {
        /// The filesystem profile.
        profile: String,
        /// The workspace-relative path.
        path: String,
    },
    /// One probe of the read-only report, for a surface that bounds each
    /// probe's time itself. It never scans every database page.
    Probe {
        /// The probe to run.
        probe: DoctorProbe,
        /// The caller's environment, which the check judges.
        env: Env,
        /// The host reads above this crate.
        host: Arc<dyn Host>,
    },
    /// Whether the store database accepts writes: a health probe that
    /// mutates nothing.
    StoreWritable,
}

/// The repairs `orbit doctor` can run before its checks, in the order they run.
#[derive(Default)]
pub struct Repairs {
    /// Restore missing review-task pull request references.
    pub missing_pr_refs: bool,
    /// Restrict writable state directories to owner-only access.
    pub state_directory_permissions: bool,
    /// Clear dead holder records from lock files.
    pub stale_locks: bool,
    /// Release conclusively stale task reservations.
    pub stale_task_locks: bool,
    /// Remove retired graph state.
    pub graph: bool,
    /// Retire deprecated definition artifacts Orbit wrote.
    pub stale_artifacts: bool,
    /// Delete orphaned task-store partitions. Requires `confirm`.
    pub orphan_task_stores: bool,
    /// Delete unreferenced automation attempt pins.
    pub automation_pins: bool,
    /// Remove retired config keys, backing each file up.
    pub retired_config_keys: bool,
}

/// What one repair did.
pub enum Repaired {
    /// The review tasks whose pull request references were restored.
    MissingPrRefs(Vec<String>),
    /// State directories restricted to owner-only access.
    StateDirectoryPermissions(usize),
    /// Stale lock holder records cleared.
    StaleLocks(usize),
    /// Stale task reservations released.
    StaleTaskLocks(usize),
    /// Retired graph locations removed.
    Graph(usize),
    /// Deprecated definition artifacts retired.
    StaleArtifacts(usize),
    /// The orphaned task-store partitions removed.
    OrphanTaskStores(OrphanTaskStoreRemoval),
    /// The automation attempt pins reclaimed and kept.
    AutomationPins(AttemptPinCleanup),
    /// The config keys removed and written.
    RetiredConfigKeys(RetiredConfigKeyRepair),
}

/// What a doctor command reported.
pub enum Outcome {
    /// The checks after any repairs.
    Report {
        /// Where the resource throttle reads the CPU budget from.
        cpu_source: String,
        /// Every check's rows, in report order.
        rows: Vec<WorkspaceDoctorResult>,
    },
    /// The admission receipt storage.
    AdmissionReceipts {
        /// Receipts compacted to tombstones by this command.
        compacted_receipts: u64,
        /// The storage after any compaction.
        storage: AdmissionReceiptStorage,
    },
    /// The executor definitions and what dispatch would find for each.
    Providers(Providers),
    /// The policy's verdicts.
    FsAccess {
        /// The read verdict.
        read: FsCheckResult,
        /// The modify verdict.
        modify: FsCheckResult,
    },
    /// One probe's rows.
    Probed(Vec<WorkspaceDoctorResult>),
    /// What the store database answered the write probe.
    StoreWritable(String),
}

/// Each executor definition, the launcher dispatch finds for it, and the
/// host facts its row reports.
pub struct Providers {
    /// The executor definitions, each with the launcher found for its
    /// `command`.
    pub executors: Vec<(ExecutorDef, Option<PathBuf>)>,
    /// A fresh Bubblewrap probe, when an executor uses `linux-bwrap`.
    pub linux_probe: Option<BwrapProbeOutcome>,
    /// The provider-limit rows, each with the provider it concerns.
    pub limit_findings: Vec<(Option<String>, WorkspaceDoctorResult)>,
}

/// Why a doctor command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The command refused or failed.
    #[error(transparent)]
    Flow(#[from] OrbitError),
}

impl From<Error> for crate::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Flow(error) => error,
        }
    }
}

struct DoctorApp<'a>(std::marker::PhantomData<&'a ()>);

impl<'a> App for DoctorApp<'a> {
    type Command = Command<'a>;
    type Outcome = Outcome;
    type Error = Error;

    /// The row the command line recorded before: the `doctor` noun as an
    /// admin action, never decided against a governance row.
    fn operation(command: &Command<'a>) -> Operation {
        let operation = match command {
            Command::Report { .. } => Operation::command("doctor", "")
                .without_recorded_subcommand()
                .target(Some("workspace"), None),
            Command::AdmissionReceipts { .. } => {
                Operation::command("doctor", "admission-receipts").target(Some("workspace"), None)
            }
            Command::Providers => {
                Operation::command("doctor", "providers").target(Some("executor"), None)
            }
            Command::FsAccess { profile, .. } => Operation::command("doctor", "fs-access")
                .target(Some("policy"), Some(profile.clone())),
            Command::Probe { .. } => return Operation::dashboard_read("doctor", "probe"),
            Command::StoreWritable => {
                return Operation::dashboard_read("doctor", "store-writable");
            }
        };
        operation.audit_role("admin").without_command_row()
    }

    fn run(core: &Core, command: Command<'a>) -> Result<Outcome, Error> {
        Ok(match command {
            Command::Report {
                repairs,
                confirm,
                deep,
                env,
                host,
                repaired,
            } => run::report(
                core,
                run::Report {
                    repairs,
                    confirm,
                    deep,
                    env: &env,
                    host: host.as_ref(),
                    repaired,
                },
            )?,
            Command::AdmissionReceipts { compact } => run::admission_receipts(core, compact)?,
            Command::Providers => Outcome::Providers(run::providers(core)?),
            Command::FsAccess { profile, path } => run::fs_access(core, &profile, &path)?,
            Command::Probe { probe, env, host } => {
                Outcome::Probed(probe.run(core.runtime(), &env, false, host.as_ref()))
            }
            Command::StoreWritable => {
                Outcome::StoreWritable(core.runtime().health_check_store_writable()?)
            }
        })
    }
}

/// Run one doctor command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command<'_>) -> Result<Outcome, crate::Error> {
    exec::run::<DoctorApp<'_>>(core, command)
}

#[cfg(test)]
mod tests;
