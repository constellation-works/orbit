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
    after_help = "A script is a manifest at `.orbit/resources/scripts/<name>.yaml` whose\n\
                  `entry` names a program file in the workspace. A job step names it\n\
                  as `target: script:<name>`. Grants are host-local consent bound to\n\
                  the manifest and entry digests: editing either file makes the grant\n\
                  stale until an operator grants again. A granted script step runs on\n\
                  Linux under a sandbox, with network and environment variables only\n\
                  as its grant allows; other hosts refuse it."
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
