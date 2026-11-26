use crate::command::ExecuteCore;
use clap::{Args, Subcommand};
use orbit_common::Env;
use orbit_core::Core;

use crate::command::CommandOut;

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
and window rules: installed orbit-orchestrate skill `references/orchestration-dispatch.md`
and `references/orchestration-drain-control.md`.
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

impl RunCommand {
    pub(crate) fn execute_with_env(self, core: &Core, env: &Env) -> CommandOut {
        self.command.execute_with_env(core, env)
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

impl RunSubcommand {
    fn execute_with_env(self, core: &Core, env: &Env) -> CommandOut {
        match self {
            RunSubcommand::Auto(command) => command.execute(core),
            RunSubcommand::Ship(command) => command.execute(core),
            RunSubcommand::TaskPilot(command) => command.execute(core),
            RunSubcommand::Readiness(command) => command.execute(core),
            RunSubcommand::Profiles(command) => command.execute(core),
            RunSubcommand::History(command) => command.execute(core),
            RunSubcommand::Show(command) => command.execute(core),
            RunSubcommand::Logs(command) => command.execute_with_env(core, env),
            RunSubcommand::Events(command) => command.execute(core),
            RunSubcommand::Trace(command) => command.execute(core),
            RunSubcommand::Settlements(command) => command.execute(core),
            RunSubcommand::SettlementsHydrate(command) => command.execute(core),
            RunSubcommand::Cancel(command) => command.execute(core),
            RunSubcommand::Concurrency(command) => command.execute(core),
            RunSubcommand::Extend(command) => command.execute(core),
            RunSubcommand::Job(command) => {
                command.submit(core, orbit_core::app::run::JobEntry::RunJob)
            }
        }
    }
}
