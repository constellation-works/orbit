use clap::Args;
use orbit_core::Core;
use orbit_core::app::run::{Command, exec};
use serde_json::json;

use crate::command::{CommandOut, Payload};

#[derive(Args)]
pub struct ScriptRevokeArgs {
    /// Script name.
    pub name: String,
}

impl ScriptRevokeArgs {
    pub(super) fn execute(self, core: &Core) -> CommandOut {
        exec(core, Command::script_revoke(&self.name))?;
        let doc = json!({"name": self.name, "revoked": true});
        Ok(Payload::detail(doc, format!("revoked script grant: {}\n", self.name)).into())
    }
}
