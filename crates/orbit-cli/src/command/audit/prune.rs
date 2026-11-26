use clap::Args;
use orbit_core::Core;
use orbit_core::app::audit::{Command, Outcome, exec};
use serde_json::json;

use crate::command::{CommandOut, ExecuteCore, Payload};

#[derive(Args)]
pub struct AuditPruneArgs {
    /// Prune events older than this duration (e.g. "90d", "1h")
    #[arg(long)]
    pub older_than: String,
    /// Confirm permanent deletion of matching audit rows
    #[arg(long)]
    pub confirm: bool,
}

impl ExecuteCore for AuditPruneArgs {
    fn execute(self, core: &Core) -> CommandOut {
        let Outcome::Pruned(pruned) = exec(
            core,
            Command::Prune {
                older_than: self.older_than,
                confirm: self.confirm,
            },
        )?
        else {
            unreachable!("audit prune returned an unrelated outcome");
        };
        Ok(Payload::detail(
            json!({ "pruned": pruned }),
            format!("Pruned {pruned} audit events"),
        )
        .into())
    }
}
