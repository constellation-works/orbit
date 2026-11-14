use clap::{Args, Subcommand};
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute};

use super::auto;
use super::cancel::RunCancelArgs;
use super::concurrency::RunConcurrencyArgs;
use super::events::RunEventsArgs;
use super::extend::RunExtendArgs;
use super::history::RunHistoryArgs;
use super::hydrate_settlements::RunHydrateSettlementsArgs;
use super::job::JobRunArgs;
use super::logs::RunLogsArgs;
use super::profiles::RunProfilesArgs;
use super::readiness::ReadinessCommand;
use super::settlements::RunSettlementsArgs;
use super::ship;
use super::show::RunShowArgs;
use super::task_pilot;
use super::trace::RunTraceArgs;

const RUN_AFTER_HELP: &str = "\
Examples:
  orbit run auto --for 2h --concurrency 8
  orbit run ship <task_id>
  orbit run job <job_id> [--input key=value] [--json]
  orbit run history --state failed --since 24h
  orbit run show <run_id>
  orbit run cancel <run_id> --confirm

`orbit run <COMMAND> -h` lists a command's flags. Drain admission, concurrency
and window rules: installed orbit-orchestrate skill `references/orchestration.md`.
";

#[derive(Args)]
#[command(
    about = "Run workflows, drain the backlog, and inspect runs",
    arg_required_else_help = true,
    subcommand_required = true,
    override_usage = "orbit run <COMMAND>",
    after_help = RUN_AFTER_HELP
)]
pub struct RunCommand {
    #[command(subcommand)]
    pub command: RunSubcommand,
}

impl Execute for RunCommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        self.command.execute(runtime)
    }
}

#[derive(Subcommand)]
pub enum RunSubcommand {
    /// Drain the workspace backlog for a window
    Auto(auto::AutoCommand),
    /// Ship backlog or explicitly selected tasks through the gated task pipeline
    Ship(ship::ShipCommand),
    /// Preflight proposed/backlog tasks and persist validated selectors
    #[command(name = "task-pilot")]
    TaskPilot(task_pilot::TaskPilotCommand),
    /// Explain why backlog tasks can or cannot start in auto-drain
    Readiness(ReadinessCommand),
    /// Show what each kind of delivery costs on this host, minute by minute
    Profiles(RunProfilesArgs),
    /// Show recent job runs filtered by job, task, state or time
    History(RunHistoryArgs),
    /// Show structured state and step summary for a job run
    Show(RunShowArgs),
    /// Print raw stdout/stderr captured for a job run
    Logs(RunLogsArgs),
    /// Show audit events recorded for a job run
    Events(RunEventsArgs),
    /// Show audit event parent/child trace for a job run
    Trace(RunTraceArgs),
    /// Show failure and release settlements claimed leaves sent this owner
    Settlements(RunSettlementsArgs),
    /// Keep a settled claim's leaf run evidence, read from its follower (operator)
    #[command(name = "settlements-hydrate")]
    SettlementsHydrate(RunHydrateSettlementsArgs),
    /// Cancel a pending or running job run
    Cancel(RunCancelArgs),
    /// Retune a running drain's admission ceiling
    Concurrency(RunConcurrencyArgs),
    /// Lengthen a running drain's window, and optionally retune it
    Extend(RunExtendArgs),
    /// Run an arbitrary job by ID
    Job(JobRunArgs),
}

impl Execute for RunSubcommand {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        match self {
            RunSubcommand::Auto(command) => command.execute(runtime),
            RunSubcommand::Ship(command) => command.execute(runtime),
            RunSubcommand::TaskPilot(command) => command.execute(runtime),
            RunSubcommand::Readiness(command) => command.execute(runtime),
            RunSubcommand::Profiles(command) => command.execute(runtime),
            RunSubcommand::History(command) => command.execute(runtime),
            RunSubcommand::Show(command) => command.execute(runtime),
            RunSubcommand::Logs(command) => command.execute(runtime),
            RunSubcommand::Events(command) => command.execute(runtime),
            RunSubcommand::Trace(command) => command.execute(runtime),
            RunSubcommand::Settlements(command) => command.execute(runtime),
            RunSubcommand::SettlementsHydrate(command) => command.execute(runtime),
            RunSubcommand::Cancel(command) => command.execute(runtime),
            RunSubcommand::Concurrency(command) => command.execute(runtime),
            RunSubcommand::Extend(command) => command.execute(runtime),
            RunSubcommand::Job(command) => command.execute(runtime),
        }
    }
}
