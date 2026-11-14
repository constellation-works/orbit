pub mod agents;
pub mod application;
pub mod attention;
pub mod audit;
pub mod auto_task;
pub mod automation;
pub mod browser_kit;
pub mod clock;
pub mod codebase;
pub mod config;
pub mod decision;
pub mod doctor;
pub mod evolution;
pub mod experiment;
pub mod friction;
pub mod gc;
pub mod host;
pub(crate) mod host_route;
pub mod init;
pub mod job;
pub mod locks;
pub mod log;
pub mod mcp;
pub mod memory;
pub mod migrate;
pub mod notice;
pub mod operation;
pub mod resident;
pub use operation::args as operation_args;
pub mod plugin;
pub mod routine;
pub mod run;
pub mod script;
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

// Grouped `--help` sections come from the row tables in `help.rs`, attached in `entry/discover.rs`.
#[derive(Parser)]
#[command(name = "orbit")]
#[command(about = "Orbit CLI", version)]
#[command(disable_help_subcommand = true)]
pub struct Cli {
    /// Orbit data directory to use (overrides every other source)
    #[arg(long, global = true)]
    pub root: Option<PathBuf>,

    /// Active workspace to use: registered name, `ws_*` ID or checkout path.
    #[arg(long, global = true, value_name = "SELECTOR")]
    pub workspace: Option<String>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    // ── Setup ──
    Init(init::InitCommand),
    Workspace(workspace::WorkspaceCommand),
    Config(config::ConfigCommand),
    Host(host::HostCommand),
    Plugin(plugin::PluginCommand),
    Migrate(migrate::MigrateCommand),
    Update(update::UpdateCommand),

    // ── Work ──
    Task(Box<task::TaskCommand>),
    Friction(friction::FrictionCommand),
    Search(search::SearchCommand),

    // ── Operate ──
    Run(run::RunCommand),
    Job(job::JobCommand),
    Script(script::ScriptCommand),
    Notice(notice::NoticeCommand),
    Tool(tool::ToolCommand),
    Gc(gc::GcCommand),

    // ── Observe ──
    Attention(attention::AttentionCommand),
    Audit(audit::AuditCommand),
    Log(log::LogCommand),
    Doctor(doctor::DoctorCommand),

    // ── Analytics ──
    Agents(agents::AgentsCommand),
    Codebase(codebase::CodebaseCommand),

    // ── Scheduler ──
    Clock(clock::ClockCommand),
    Routine(routine::RoutineCommand),
    #[command(name = "auto-task")]
    AutoTask(auto_task::AutoTaskCommand),
    Automation(automation::AutomationCommand),

    // ── Services ──
    Mcp(mcp::McpCommand),
    Web(web::WebCommand),

    // ── Advanced ──
    Application(application::ApplicationCommand),
    Decision(decision::DecisionCommand),
    Evolution(evolution::EvolutionCommand),
    Experiment(experiment::ExperimentCommand),
    Memory(memory::MemoryCommand),
    Resident(resident::ResidentCommand),

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
    // Deprecated path of `orbit config browser-kit`, kept for the compatibility
    // window; each use warns on stderr.
    #[command(name = "browser-kit", hide = true)]
    BrowserKit(browser_kit::BrowserKitCommand),
    // Compatibility spelling of `orbit clock tick`.
    #[command(hide = true)]
    Sweep(sweep::SweepCommand),
}
