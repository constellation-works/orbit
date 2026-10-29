use clap::{Args, Subcommand};
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute};

use super::agent::RunAgentArgs;
use super::auto;
use super::cancel::RunCancelArgs;
use super::concurrency::RunConcurrencyArgs;
use super::events::RunEventsArgs;
use super::extend::RunExtendArgs;
use super::history::RunHistoryArgs;
use super::job::JobRunArgs;
use super::logs::RunLogsArgs;
use super::profiles::RunProfilesArgs;
use super::readiness::ReadinessCommand;
use super::settlements::RunSettlementsArgs;
use super::ship;
use super::show::RunShowArgs;
use super::task_pilot;
use super::trace::RunTraceArgs;

const RUN_AFTER_LONG_HELP: &str = "\
Owner/local drains count every in-flight child against --concurrency. A replica's pull drain
uses it as an admission ceiling with capped before-landing review credit, so it is not a hard
total live-task cap. Details: the orbit-orchestrate skill, references/orchestration.md.
";

#[derive(Args)]
#[command(
    about = "Run workflows, drain the backlog, and inspect runs",
    arg_required_else_help = true,
    subcommand_required = true,
    override_usage = "orbit run <COMMAND>",
    after_long_help = RUN_AFTER_LONG_HELP,
    help_template = "\
{about}

{usage-heading} {usage}

Workflows:
  auto        Drain the workspace backlog for a window; --stop ends new admissions
  ship        Ship backlog or explicitly selected tasks through the gated task pipeline
  task-pilot  Preflight proposed/backlog tasks and persist validated selectors
  job         Run an arbitrary job by ID
  agent       Invoke an agent on the host for exploration or debugging (operator only)

Audits:
  history    Show recent job runs filtered by job, task, state or time
  show       Show structured state and step summary for a job run
  logs       Print raw stdout/stderr captured for a job run
  events     Show audit events recorded for a job run
  trace      Show audit event parent/child trace for a job run
  settlements  Show failure and release settlements claimed leaves sent this owner

Maintenance:
  cancel       Cancel a pending or running job run
  concurrency  Retune a running drain's admission ceiling
  extend       Lengthen a running drain's window, and optionally retune it
  readiness    Explain why backlog tasks are waiting in auto-drain
  profiles     Show what each kind of delivery costs on this host, minute by minute

Run 'orbit run <command> --help' for flags and examples.

Options:
{options}
{after-help}"
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
    /// Cancel a pending or running job run
    Cancel(RunCancelArgs),
    /// Retune a running drain's admission ceiling
    Concurrency(RunConcurrencyArgs),
    /// Lengthen a running drain's window, and optionally retune it
    Extend(RunExtendArgs),
    /// Run an arbitrary job by ID
    Job(JobRunArgs),
    /// Invoke an agent on the host for exploration or debugging (operator only)
    Agent(RunAgentArgs),
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
            RunSubcommand::Cancel(command) => command.execute(runtime),
            RunSubcommand::Concurrency(command) => command.execute(runtime),
            RunSubcommand::Extend(command) => command.execute(runtime),
            RunSubcommand::Job(command) => command.execute(runtime),
            RunSubcommand::Agent(command) => command.execute(runtime),
        }
    }
}
