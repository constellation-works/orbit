pub mod activity;
pub mod audit;
pub mod auto_task;
pub mod clock;
pub mod config;
pub mod doctor;
pub mod executor;
pub mod friction;
pub mod gc;
pub mod init;
pub mod job;
pub mod locks;
pub mod log;
pub mod mcp;
pub mod migrate;
pub mod operation;
pub mod operation_args;
pub mod plugin;
pub mod policy;
pub mod routine;
pub mod run;
pub mod search;
pub mod skill;
pub mod sweep;
pub mod task;
pub mod tool;
pub mod update;
pub mod web;
pub mod workspace;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use orbit_core::{OrbitError, OrbitRuntime};

// Re-exported so a command file imports its return type from the module that
// defines the trait, rather than reaching into `output` for half of it.
pub use crate::output::payload::{Block, CommandOutput, Payload};

/// What every command body returns: the records it produced, or
/// [`CommandOutput::Silent`] when its effect was its output.
///
/// A command never writes a record to stdout and never inspects the sink;
/// `output::render` projects this into the resolved mode
/// (`docs/design/terminal-interface/specs/output-modes.md` §3, ADR-0306).
pub type CommandOut = Result<CommandOutput, OrbitError>;

pub trait Execute {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut;
}

/// Require the standard non-interactive confirmation flag before an
/// irreversible CLI operation proceeds.
pub(crate) fn require_confirmation(confirm: bool, action: &str) -> Result<(), OrbitError> {
    if confirm {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "{action} is irreversible; pass --confirm to proceed"
    )))
}

// Clap derive does not support per-variant subcommand `help_heading`
// (`next_help_heading` is args-only; `subcommand_help_heading` only renames
// the single `Commands:` block). To render grouped sections in `--help` we
// hand-roll the template below. Keep the variant order and the template's
// section order in sync when adding new commands — the variant order also
// determines where a missing-from-template command would otherwise appear.
//
// It is a named constant rather than an inline literal because `main`
// splices in a `Plugins:` section for the host's installed plugin groups
// before parsing, and clap does not hand a built template back out.
pub(crate) const ROOT_HELP_TEMPLATE: &str = "\
{name} {version}

{usage-heading} {usage}

Environment:
  init        Initialize the global Orbit root (~/.orbit)
  workspace   Manage workspaces
  config      Show or update Orbit configuration
  migrate     Apply or inspect pending .orbit layout/schema migrations
  update      Install a published Orbit release and converge to it

Knowledge:
  task        Create, update, and manage tasks
  friction    Report, list, and triage friction records
  search      Search tasks and frictions

Operate:
  run         Run a workflow (ship, job)
  gc          Inspect and explicitly reap Orbit-managed garbage

Observe:
  audit       Query the audit event log
  log         Tail the unified Orbit log feed
  doctor      Diagnose workspace health (config, database, disk, indexes)

Definitions:
  activity    View activity definitions
  job         View job definitions
  tool        View tool registry
  plugin      Install and manage Orbit plugins
  policy      View filesystem policies
  executor    View executors

Scheduler:
  clock       Inspect, control, and manually tick the machine scheduler
  sweep       Compatibility alias for `orbit clock tick`
  routine     Inspect and control scheduled routines on this machine
  auto-task   Define recurring auto-task templates (the scheduler primitive)

Services:
  mcp         Register MCP client integrations and run the MCP server
  web         Run the Orbit dashboard

Options:
{options}";

#[derive(Parser)]
#[command(name = "orbit")]
#[command(about = "Orbit CLI", version)]
#[command(
    disable_help_subcommand = true,
    help_template = ROOT_HELP_TEMPLATE
)]
pub struct Cli {
    /// Override the Orbit root directory (highest precedence)
    #[arg(long, global = true)]
    pub root: Option<PathBuf>,

    /// Select a workspace by registered name, logical ID (`ws_*`), or absolute
    /// checkout path. Distinct from `--root`, which overrides the Orbit data
    /// directory. Only active workspaces may be bound; commands fail if the
    /// workspace status is not active.
    #[arg(long, global = true, value_name = "SELECTOR")]
    pub workspace: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    // ── Environment ──
    Init(init::InitCommand),
    Workspace(workspace::WorkspaceCommand),
    Config(config::ConfigCommand),
    Migrate(migrate::MigrateCommand),
    Update(update::UpdateCommand),

    // ── Knowledge ──
    Task(Box<task::TaskCommand>),
    Friction(friction::FrictionCommand),
    Search(search::SearchCommand),

    // ── Operate ──
    Run(run::RunCommand),
    Gc(gc::GcCommand),

    // ── Observe ──
    Audit(audit::AuditCommand),
    Log(log::LogCommand),
    Doctor(doctor::DoctorCommand),

    // ── Definitions ──
    Activity(activity::ActivityCommand),
    Job(job::JobCommand),
    Tool(tool::ToolCommand),
    Plugin(plugin::PluginCommand),
    Policy(policy::PolicyCommand),
    Executor(executor::ExecutorCommand),

    // ── Scheduler ──
    Clock(clock::ClockCommand),
    Sweep(sweep::SweepCommand),
    Routine(routine::RoutineCommand),
    #[command(name = "auto-task")]
    AutoTask(auto_task::AutoTaskCommand),

    // ── Services ──
    Mcp(mcp::McpCommand),
    Web(web::WebCommand),

    // ── plugin-derived command groups ──
    //
    // Not a clap-visible variant: `orbit <ns> <verb>` is built at startup
    // from the installed manifests (`crate::plugin_cli`), parsed against the
    // augmented tree in `main`, and handed here already reduced to the tool
    // call it performs. `#[command(skip)]` keeps the derive from inventing a
    // literal `orbit plugin-group` subcommand for it.
    #[command(skip)]
    PluginGroup(Box<crate::plugin_cli::PluginGroupInvocation>),

    // ── hidden compatibility commands ──
    #[command(hide = true)]
    Skill(skill::SkillCommand),
    #[command(hide = true)]
    Logs(run::legacy_logs::LogsCommand),
    #[command(hide = true)]
    Artifacts(task::artifacts::ArtifactsCommand),
}

#[cfg(test)]
mod tests;
