//! The gc app: inspect and explicitly reap Orbit-managed garbage.
//!
//! Each [`Command`] names one garbage class. Without `confirm` a command only
//! reports what it would reclaim; with it, the command reaps, and only then is
//! the caller held to the class's `gc <class>` governance row.

use orbit_common::OrbitError;
use orbit_engine::WorktreeGcResult;

use super::exec::{self, App, Operation};
use crate::Core;
use crate::application::gc::{
    AuditGcReport, DeploymentGcResult, JournalGcReport, RunGcReport, TmpGcResult,
};

/// What the gc app can be asked to do: one variant per garbage class.
pub enum Command {
    /// Reap job-run worktrees whose task or claim has settled.
    Worktrees {
        /// Reap, rather than only report.
        confirm: bool,
        /// Restrict collection to one job run.
        run: Option<String>,
        /// Only worktrees older than this many hours.
        older_than_hours: Option<u64>,
        /// Measure reclaimable bytes on a report.
        estimate_bytes: bool,
        /// Reclaim `worktree.reclaim` paths and keep the checkout.
        reclaim: bool,
    },
    /// Reclaim this checkout's scratch contents.
    Tmp {
        /// Reap, rather than only report.
        confirm: bool,
    },
    /// Remove deployments and backups beyond the newest `keep` of each.
    Deployments {
        /// Reap, rather than only report.
        confirm: bool,
        /// Newest entries to keep in each class.
        keep: usize,
    },
    /// Prune old audit rows and the blobs no remaining row names.
    Audit {
        /// Reap, rather than only report.
        confirm: bool,
        /// Override `retention.audit_days`.
        older_than_days: Option<u32>,
    },
    /// Drop the pipeline state of old terminal runs.
    Runs {
        /// Reap, rather than only report.
        confirm: bool,
        /// Override `retention.runs_days`.
        older_than_days: Option<u32>,
    },
    /// Delete old applied task commit journal rows.
    Journal {
        /// Reap, rather than only report.
        confirm: bool,
        /// Override the journal's retention.
        older_than_days: Option<u32>,
    },
}

impl Command {
    /// The garbage class, as the command line and the audit row name it.
    fn class(&self) -> &'static str {
        match self {
            Self::Worktrees { .. } => "worktrees",
            Self::Tmp { .. } => "tmp",
            Self::Deployments { .. } => "deployments",
            Self::Audit { .. } => "audit",
            Self::Runs { .. } => "runs",
            Self::Journal { .. } => "journal",
        }
    }

    fn confirm(&self) -> bool {
        match self {
            Self::Worktrees { confirm, .. }
            | Self::Tmp { confirm }
            | Self::Deployments { confirm, .. }
            | Self::Audit { confirm, .. }
            | Self::Runs { confirm, .. }
            | Self::Journal { confirm, .. } => *confirm,
        }
    }
}

/// What a command reported or reaped.
pub enum Outcome {
    /// The worktree report.
    Worktrees(WorktreeGcResult),
    /// The scratch report.
    Tmp(TmpGcResult),
    /// The deployment report.
    Deployments(DeploymentGcResult),
    /// The audit retention report.
    Audit(AuditGcReport),
    /// The run retention report.
    Runs(RunGcReport),
    /// The journal retention report.
    Journal(JournalGcReport),
}

/// Why a gc command failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The collection refused or failed.
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

struct GcApp;

impl App for GcApp {
    type Command = Command;
    type Outcome = Outcome;
    type Error = Error;

    fn operation(command: &Command) -> Operation {
        let class = command.class();
        let operation =
            Operation::command("gc", class).target(Some("garbage"), Some(class.to_string()));
        // Only reaping is governed: a report is the non-destructive form.
        if command.confirm() {
            operation
        } else {
            operation.without_command_row()
        }
    }

    fn run(core: &Core, command: Command) -> Result<Outcome, Error> {
        let runtime = core.runtime();
        Ok(match command {
            Command::Worktrees {
                confirm,
                run,
                older_than_hours,
                estimate_bytes,
                reclaim,
            } => Outcome::Worktrees(runtime.gc_worktrees(
                confirm,
                run,
                older_than_hours,
                estimate_bytes,
                reclaim,
            )?),
            Command::Tmp { confirm } => Outcome::Tmp(runtime.gc_tmp(confirm)?),
            Command::Deployments { confirm, keep } => {
                Outcome::Deployments(runtime.gc_deployments(confirm, keep)?)
            }
            Command::Audit {
                confirm,
                older_than_days,
            } => Outcome::Audit(runtime.gc_audit(confirm, older_than_days)?),
            Command::Runs {
                confirm,
                older_than_days,
            } => Outcome::Runs(runtime.gc_runs(confirm, older_than_days)?),
            Command::Journal {
                confirm,
                older_than_days,
            } => Outcome::Journal(runtime.gc_journal(confirm, older_than_days)?),
        })
    }
}

/// Run one gc command: authorize it, audit it and return its outcome.
pub fn exec(core: &Core, command: Command) -> Result<Outcome, crate::Error> {
    exec::run::<GcApp>(core, command)
}
