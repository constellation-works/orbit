use crate::command::locks::LocksContentionArgs;
use clap::{ArgMatches, Args, Command, FromArgMatches, Subcommand};
use orbit_common::governance::task::{TASK, TaskVerb};
use orbit_core::OrbitRuntime;

use super::super::operation_args::Invocation;
use crate::command::{CommandOut, Execute};

use super::archive::TaskArchiveArgs;
use super::export::TaskExportArgs;
use super::flow::TaskFlowArgs;
use super::import::TaskImportArgs;
use super::publication::TaskPublicationCommand;
use super::recheck_blocked::TaskRecheckBlockedArgs;
use super::reindex::TaskReindexArgs;
use super::settle_merged::TaskSettleMergedArgs;

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
    // Task reads, writes, artifacts, locks and the review verbs, derived
    // from the task operation registry.
    #[command(flatten)]
    Operation(TaskInvocation),
    /// Archive a task
    Archive(TaskArchiveArgs),
    /// Show filed-vs-closed rates over time — whether the backlog is draining
    Flow(TaskFlowArgs),
    /// Re-check tasks blocked by a missing provider launcher or transient
    /// infrastructure; `--confirm` returns the cleared ones to backlog
    RecheckBlocked(TaskRecheckBlockedArgs),
    /// Settle tasks to done whose delivery pull request was merged outside
    /// Orbit
    SettleMerged(TaskSettleMergedArgs),
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
            TaskSubcommand::Operation(invocation) => invocation.execute(runtime),
            TaskSubcommand::Flow(args) => args.execute(runtime),
            TaskSubcommand::RecheckBlocked(args) => args.execute(runtime),
            TaskSubcommand::SettleMerged(args) => args.execute(runtime),
            TaskSubcommand::Export(args) => args.execute(runtime),
            TaskSubcommand::Import(args) => args.execute(runtime),
            TaskSubcommand::Publication(command) => command.execute(runtime),
            TaskSubcommand::Reindex(args) => args.execute(runtime),
        }
    }
}

/// Migrated operations plus the existing contention command, which has no tool.
pub enum TaskInvocation {
    Registry(Invocation<TaskVerb>),
    Contention(LocksContentionArgs),
}

impl Subcommand for TaskInvocation {
    fn augment_subcommands(cmd: Command) -> Command {
        Invocation::<TaskVerb>::augment_subcommands(cmd).mut_subcommand("locks", |cmd| {
            let mut cmd = cmd;
            for (index, spec) in TASK
                .operations
                .iter()
                .filter(|spec| spec.cli_path.first() == Some(&"locks"))
                .enumerate()
            {
                let order = if index == 0 { 0 } else { index + 1 };
                cmd = cmd.mut_subcommand(spec.cli_path[1], |cmd| cmd.display_order(order));
            }
            cmd.subcommand(
                LocksContentionArgs::augment_args(Command::new("contention")).display_order(1),
            )
        })
    }
    fn augment_subcommands_for_update(cmd: Command) -> Command {
        Self::augment_subcommands(cmd)
    }
    fn has_subcommand(name: &str) -> bool {
        Invocation::<TaskVerb>::has_subcommand(name)
    }
}

impl FromArgMatches for TaskInvocation {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        if let Some(("locks", group)) = matches.subcommand()
            && let Some(("contention", leaf)) = group.subcommand()
        {
            return LocksContentionArgs::from_arg_matches(leaf).map(Self::Contention);
        }
        Invocation::from_arg_matches(matches).map(Self::Registry)
    }
    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

impl Execute for TaskInvocation {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self {
            Self::Registry(invocation) => super::operation::execute(invocation, runtime),
            Self::Contention(args) => args.execute(runtime),
        }
    }
}
