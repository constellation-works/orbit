use clap::{Args, Subcommand};
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute};

use super::agent::RunAgentArgs;
use super::auto;
use super::cancel::RunCancelArgs;
use super::concurrency::RunConcurrencyArgs;
use super::events::RunEventsArgs;
use super::history::RunHistoryArgs;
use super::job::JobRunArgs;
use super::logs::RunLogsArgs;
use super::readiness::ReadinessCommand;
use super::ship;
use super::show::RunShowArgs;
use super::sweep;
use super::trace::RunTraceArgs;
use super::triage;

const RUN_AFTER_HELP: &str = "\
Workflow entrypoints:
  orbit run auto [--for 30m]
  orbit run auto --stop
  orbit run ship [task_id ...]
  orbit run ship-sweep [--dry-run] [--json]
  orbit run triage [task_id ...]
  orbit run job <job_id> [--input key=value] [--json] [--debug]
  orbit run agent <prompt> [--cwd DIR] [--crew NAME] [--timeout SECONDS] [--provider-sandbox MODE]

Run history:
  orbit run history [--limit 50]
  orbit run history -j <job_id>
  orbit run show [run_id] [-s step_id] [--json]
  orbit run logs [run_id] [-s step_id] [--json]
  orbit run events [run_id] [-s step_id] [--type event_type] [--json]
  orbit run trace [run_id] [--json]

Maintenance:
  orbit run cancel <run_id>
  orbit run concurrency <run_id> --set 7
";

#[derive(Args)]
#[command(
    about = "Run a job workflow (supports run ship / job)",
    arg_required_else_help = true,
    subcommand_required = true,
    override_usage = "orbit run <COMMAND>",
    after_help = RUN_AFTER_HELP,
    help_template = "\
{about}

{usage-heading} {usage}

Workflows:
  auto        Drain the workspace backlog for a window (loose leaves, plus one epic); --stop ends new admissions
  ship        Ship backlog or explicitly selected tasks through the gated task pipeline
  ship-sweep  Dispatch ship runs in every registered workspace with ready backlog tasks
  triage      Triage tasks blocked by failed runs; re-backlog environmental failures
  readiness   Explain why backlog tasks are waiting in auto-drain
  job         Run an arbitrary job by ID
  agent       Invoke an agent on the host for exploration or debugging (operator only)

Audits:
  history    Show recent job runs, optionally filtered to one job
  show       Show structured state and step summary for a job run
  logs       Print raw stdout/stderr captured for a job run
  events     Show audit events recorded for a job run
  trace      Show audit event parent/child trace for a job run

Maintenance:
  cancel       Cancel a pending or running job run
  concurrency  Change how many tasks a running drain keeps in flight

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
    /// Drain the workspace backlog for a window (loose leaves, plus one epic)
    Auto(auto::AutoCommand),
    /// Ship backlog or explicitly selected tasks through the gated task pipeline
    Ship(ship::ShipCommand),
    /// Deprecated alias for `orbit run ship --mode local`
    #[command(name = "ship-local", hide = true)]
    ShipLocal(ship::LegacyShipLocalCommand),
    /// Dispatch ship runs in every registered workspace with ready backlog tasks
    #[command(name = "ship-sweep")]
    ShipSweep(sweep::ShipSweepCommand),
    /// Triage tasks blocked by failed runs; re-backlog environmental failures
    Triage(triage::TriageCommand),
    /// Explain why backlog tasks can or cannot start in auto-drain
    Readiness(ReadinessCommand),
    /// Show recent job runs, optionally filtered to one job
    History(RunHistoryArgs),
    /// Show structured state and step summary for a job run
    Show(RunShowArgs),
    /// Print raw stdout/stderr captured for a job run
    Logs(RunLogsArgs),
    /// Show audit events recorded for a job run
    Events(RunEventsArgs),
    /// Show audit event parent/child trace for a job run
    Trace(RunTraceArgs),
    /// Cancel a pending or running job run
    Cancel(RunCancelArgs),
    /// Change how many tasks a running drain keeps in flight
    Concurrency(RunConcurrencyArgs),
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
            RunSubcommand::ShipLocal(command) => command.execute(runtime),
            // Normally dispatched before runtime init (see main.rs); the
            // registry-driven sweep never uses the cwd-derived runtime.
            RunSubcommand::ShipSweep(command) => command.execute_without_runtime(),
            RunSubcommand::Triage(command) => command.execute(runtime),
            RunSubcommand::Readiness(command) => command.execute(runtime),
            RunSubcommand::History(command) => command.execute(runtime),
            RunSubcommand::Show(command) => command.execute(runtime),
            RunSubcommand::Logs(command) => command.execute(runtime),
            RunSubcommand::Events(command) => command.execute(runtime),
            RunSubcommand::Trace(command) => command.execute(runtime),
            RunSubcommand::Cancel(command) => command.execute(runtime),
            RunSubcommand::Concurrency(command) => command.execute(runtime),
            RunSubcommand::Job(command) => command.execute(runtime),
            RunSubcommand::Agent(command) => command.execute(runtime),
        }
    }
}
