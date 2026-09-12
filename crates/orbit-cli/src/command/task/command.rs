use clap::{Args, Subcommand};
use orbit_core::OrbitRuntime;

use crate::command::locks::LocksCommand;
use crate::command::{CommandOut, Execute};

use super::add::TaskAddArgs;
use super::archive::TaskArchiveArgs;
use super::artifact::TaskArtifactCommand;
use super::export::TaskExportArgs;
use super::flow::TaskFlowArgs;
use super::import::TaskImportArgs;
use super::lint::TaskLintArgs;
use super::list::TaskListArgs;
use super::publication::TaskPublicationCommand;
use super::reindex::TaskReindexArgs;
use super::show::TaskShowArgs;
use super::update::TaskUpdateArgs;

/// Grouped `orbit task` help, rendered the same way `orbit run` and the root
/// command render theirs: a hand-rolled template, because clap's derive has no
/// per-variant `help_heading`. Fourteen ungrouped rows read as a wall; the
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
  show         Show one task in detail, by ID, across registered workspaces
  artifact     Manage task artifact files

Health:
  lint         Lint tasks for stale paths and vague acceptance criteria
  flow         Show filed-vs-closed rates over time — is the backlog draining?
  locks        Inspect, reserve, and release the file locks that gate dispatch

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
    about = "Create, update, and manage tasks",
    override_usage = "orbit task <COMMAND>",
    help_template = TASK_HELP_TEMPLATE,
    after_help = TASK_AFTER_HELP
)]
pub struct TaskCommand {
    #[command(subcommand)]
    pub command: TaskSubcommand,
}

impl Execute for TaskCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        self.command.execute(runtime)
    }
}

#[derive(Subcommand)]
pub enum TaskSubcommand {
    /// Create a new task
    Add(TaskAddArgs),
    /// Update task fields, or take the next approval step with `--approve`
    /// (proposed -> backlog, review -> done)
    Update(TaskUpdateArgs),
    /// Archive a task
    Archive(TaskArchiveArgs),
    /// List tasks with optional filters
    List(TaskListArgs),
    /// Show detailed information about a task, found by ID with an optional
    /// workspace filter
    Show(TaskShowArgs),
    /// Manage task artifact files
    Artifact(TaskArtifactCommand),
    /// Lint tasks for stale paths and vague acceptance criteria; `--fix` prunes stale context files
    Lint(TaskLintArgs),
    /// Show filed-vs-closed rates over time — whether the backlog is draining
    Flow(TaskFlowArgs),
    /// Inspect, reserve, and release task file locks
    Locks(LocksCommand),
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
            TaskSubcommand::Add(args) => args.execute(runtime),
            TaskSubcommand::Update(args) => args.execute(runtime),
            TaskSubcommand::Archive(args) => args.execute(runtime),
            TaskSubcommand::List(args) => args.execute(runtime),
            TaskSubcommand::Show(args) => args.execute(runtime),
            TaskSubcommand::Artifact(cmd) => cmd.execute(runtime),
            TaskSubcommand::Lint(args) => args.execute(runtime),
            TaskSubcommand::Flow(args) => args.execute(runtime),
            TaskSubcommand::Locks(cmd) => cmd.execute(runtime),
            TaskSubcommand::Export(args) => args.execute(runtime),
            TaskSubcommand::Import(args) => args.execute(runtime),
            TaskSubcommand::Publication(command) => command.execute(runtime),
            TaskSubcommand::Reindex(args) => args.execute(runtime),
        }
    }
}
