use clap::{Args, Subcommand};
use orbit_core::Core;

use super::grant::ScriptGrantArgs;
use super::history::ScriptHistoryArgs;
use super::list::ScriptListArgs;
use super::revoke::ScriptRevokeArgs;
use super::show::ScriptShowArgs;
use crate::command::{CommandOut, ExecuteCore};

#[derive(Args)]
#[command(
    about = "Grant and inspect workspace scripts a job step may run",
    arg_required_else_help = true,
    subcommand_required = true,
    after_help = "A script is a manifest at `.orbit/resources/scripts/<name>.yaml` that a job \
                  step names as `target: script:<name>`. Editing the manifest or its entry \
                  makes the grant stale until an operator grants again.\n\n\
                  Examples:\n  \
                  orbit script list\n  \
                  orbit script grant <name>",
    after_long_help = "A script is a manifest at `.orbit/resources/scripts/<name>.yaml` whose \
                       `entry` names a program file in the workspace. A job step names it as \
                       `target: script:<name>`. Grants are host-local consent bound to the \
                       manifest and entry digests: editing either file makes the grant stale \
                       until an operator grants again.\n\n\
                       Where a granted step runs, and which network and environment variables \
                       the grant allows: installed orbit-setup skill \
                       `references/automation-scripts.md`.\n\n\
                       Examples:\n  \
                       orbit script list\n  \
                       orbit script grant <name>"
)]
pub struct ScriptCommand {
    #[command(subcommand)]
    pub command: ScriptSubcommand,
}

#[derive(Subcommand)]
pub enum ScriptSubcommand {
    /// List workspace scripts with their entry, digest and grant state
    List(ScriptListArgs),
    /// Show one script's manifest facts, grant and staleness
    Show(ScriptShowArgs),
    /// Grant one script at its current manifest and entry digests
    Grant(ScriptGrantArgs),
    /// Revoke one script's grant on this host
    Revoke(ScriptRevokeArgs),
    /// List recent attempts of one script, refusals included
    History(ScriptHistoryArgs),
}

impl ExecuteCore for ScriptCommand {
    fn execute(self, core: &Core) -> CommandOut {
        match self.command {
            ScriptSubcommand::List(args) => args.execute(core),
            ScriptSubcommand::Show(args) => args.execute(core),
            ScriptSubcommand::Grant(args) => args.execute(core),
            ScriptSubcommand::Revoke(args) => args.execute(core),
            ScriptSubcommand::History(args) => args.execute(core),
        }
    }
}
