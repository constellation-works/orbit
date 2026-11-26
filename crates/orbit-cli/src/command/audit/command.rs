use clap::{Args, Subcommand};
use orbit_core::Core;

use crate::command::{CommandOut, ExecuteCore};

use super::export::AuditExportArgs;
use super::list::AuditListArgs;
use super::prune::AuditPruneArgs;
use super::show::AuditShowArgs;
use super::stats::AuditStatsArgs;

#[derive(Args)]
#[command(about = "Query the audit event log")]
pub struct AuditCommand {
    #[command(subcommand)]
    pub command: AuditSubcommand,
}

impl ExecuteCore for AuditCommand {
    fn execute(self, core: &Core) -> CommandOut {
        self.command.execute(core)
    }
}

#[derive(Subcommand)]
pub enum AuditSubcommand {
    /// List audit events
    List(AuditListArgs),
    /// Show a single audit event
    Show(AuditShowArgs),
    /// Prune old audit events
    Prune(AuditPruneArgs),
    /// Export audit events to file
    Export(AuditExportArgs),
    /// Show audit event statistics
    Stats(AuditStatsArgs),
}

impl ExecuteCore for AuditSubcommand {
    fn execute(self, core: &Core) -> CommandOut {
        match self {
            AuditSubcommand::List(args) => args.execute(core),
            AuditSubcommand::Show(args) => args.execute(core),
            AuditSubcommand::Prune(args) => args.execute(core),
            AuditSubcommand::Export(args) => args.execute(core),
            AuditSubcommand::Stats(args) => args.execute(core),
        }
    }
}
