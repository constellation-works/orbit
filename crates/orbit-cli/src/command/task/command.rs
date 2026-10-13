use clap::{Args, Subcommand};
use orbit_common::governance::task::{TASK, TASK_HOST_HELP, TaskVerb};
use orbit_core::OrbitRuntime;

use super::super::operation_args::Invocation;
use crate::command::locks::LocksCommand;
use crate::command::{CommandOut, Execute};

use super::archive::TaskArchiveArgs;
use super::artifact::TaskArtifactCommand;
use super::export::TaskExportArgs;
use super::flow::TaskFlowArgs;
use super::import::TaskImportArgs;
use super::publication::TaskPublicationCommand;
use super::recheck_blocked::TaskRecheckBlockedArgs;
use super::reconcile_review::TaskReconcileReviewCommand;
use super::reindex::TaskReindexArgs;
use super::review_reset::TaskReviewResetArgs;

/// Grouped `orbit task` help, rendered the same way `orbit run` and the root
/// command render theirs: a hand-rolled template, because clap's derive has no
/// per-variant `help_heading`. Fifteen ungrouped rows read as a wall; the
/// sections say which of them you are looking for. Keep the variant order in
/// `TaskSubcommand` matching the section order below — the order decides where
/// a command would land if it were ever missing from the template.
const TASK_HELP_TEMPLATE: &str = "\
{about}

{usage-heading} {usage}

Tasks:
  add          Create a new task
  update       Update task fields; `--approve` takes the next approval step
  archive      Archive a task
  list         List tasks with optional filters
  eligible     List backlog/proposed tasks that collide with no in-flight work
  show         Show one task in detail, by ID, across registered workspaces
  artifact     Manage task artifact files

Health:
  lint         Lint tasks for context-file and acceptance-criteria problems
  flow         Show filed-vs-closed rates over time — is the backlog draining?
  locks        Inspect, reserve, and release the file locks that gate dispatch
  review-reset Reset one review lineage budget with an audited reason
  reconcile-review
               Validate and review a recovered delivery's changed merged head
  recheck-blocked
               Requeue tasks blocked by a provider launcher that now resolves

Bundles:
  export       Export task bundles to a portable tar.zst archive
  import       Import task bundles from a tar.zst archive
  publication  Publish, inspect, diagnose, or restore task snapshots
  reindex      Rebuild the registry index from on-disk task bundles

Options:
{options}
{after-help}";

const TASK_AFTER_HELP: &str = "\
Lifecycle:
  orbit task update <id> --approve                  proposed -> backlog, review -> done
  orbit task update <id> --status in-progress       take the work
  orbit task update <id> --status review            hand it off

Run `orbit task <COMMAND> --help` for a command's own options and examples.";

#[derive(Args)]
#[command(
    about = TASK.cli_about,
    override_usage = "orbit task <COMMAND>",
    help_template = TASK_HELP_TEMPLATE,
    after_help = TASK_AFTER_HELP
)]
pub struct TaskCommand {
    #[command(subcommand)]
    pub command: TaskSubcommand,
}

/// Host selection shared by task commands that can deliver a tool call.
#[derive(Args)]
pub(crate) struct TaskHostArgs {
    #[arg(long, global = true, value_name = "HOST", help = TASK_HOST_HELP)]
    pub host: Option<String>,
}

impl Execute for TaskCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        self.command.execute(runtime)
    }
}

#[derive(Subcommand)]
pub enum TaskSubcommand {
    // `add`, `update`, `list`, `eligible`, `show` and `lint`, derived from
    // the task operation registry.
    #[command(flatten)]
    Operation(Invocation<TaskVerb>),
    /// Archive a task
    Archive(TaskArchiveArgs),
    /// Manage task artifact files
    Artifact(TaskArtifactCommand),
    /// Show filed-vs-closed rates over time — whether the backlog is draining
    Flow(TaskFlowArgs),
    /// Inspect, reserve, and release task file locks
    Locks(LocksCommand),
    /// Reset a review lineage budget with an audited operator reason
    ReviewReset(TaskReviewResetArgs),
    /// Validate and independently review the already-merged head of a
    /// recovered follower delivery so its task can complete from review
    ReconcileReview(TaskReconcileReviewCommand),
    /// Re-check tasks blocked by a missing provider launcher; `--confirm` returns
    /// the ones whose launcher now resolves to backlog
    RecheckBlocked(TaskRecheckBlockedArgs),
    /// Export task bundles to a portable tar.zst archive
    Export(TaskExportArgs),
    /// Import task bundles from a tar.zst archive
    Import(TaskImportArgs),
    /// Publish, inspect, diagnose, or deliberately restore task snapshots
    Publication(TaskPublicationCommand),
    /// Rebuild the registry index from on-disk task bundles
    Reindex(TaskReindexArgs),
}

impl Execute for TaskSubcommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self {
            TaskSubcommand::Archive(args) => args.execute(runtime),
            TaskSubcommand::Operation(invocation) => super::operation::execute(invocation, runtime),
            TaskSubcommand::Artifact(cmd) => cmd.execute(runtime),
            TaskSubcommand::Flow(args) => args.execute(runtime),
            TaskSubcommand::Locks(cmd) => cmd.execute(runtime),
            TaskSubcommand::RecheckBlocked(args) => args.execute(runtime),
            TaskSubcommand::ReviewReset(args) => args.execute(runtime),
            TaskSubcommand::ReconcileReview(command) => command.execute(runtime),
            TaskSubcommand::Export(args) => args.execute(runtime),
            TaskSubcommand::Import(args) => args.execute(runtime),
            TaskSubcommand::Publication(command) => command.execute(runtime),
            TaskSubcommand::Reindex(args) => args.execute(runtime),
        }
    }
}
