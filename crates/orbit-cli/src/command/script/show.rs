use clap::Args;
use orbit_core::Core;
use orbit_core::app::run::{Command, exec};

use super::support::script_status_json;
use crate::command::{CommandOut, Payload};

#[derive(Args)]
pub struct ScriptShowArgs {
    /// Script name.
    pub name: String,
}

impl ScriptShowArgs {
    pub(super) fn execute(self, core: &Core) -> CommandOut {
        let status = exec(core, Command::script(&self.name))?.value;
        let doc = script_status_json(&status);
        let mut text = format!("script: {}\n", status.name);
        text.push_str(&format!("grant state: {}\n", status.state));
        if let Some(asset) = &status.asset {
            text.push_str(&format!("manifest: {}\n", asset.manifest_path.display()));
            text.push_str(&format!("entry: {}\n", asset.spec.entry));
            text.push_str(&format!(
                "entry (resolved): {}\n",
                asset.entry_path.display()
            ));
            if !asset.interpreter.is_empty() {
                text.push_str(&format!("interpreter: {}\n", asset.interpreter.join(" ")));
            }
            if !asset.spec.args.is_empty() {
                text.push_str(&format!("args: {}\n", asset.spec.args.join(" ")));
            }
            text.push_str(&format!("current digest: {}\n", asset.digest));
            text.push_str(&format!(
                "current manifest digest: {}\n",
                asset.manifest_digest
            ));
            text.push_str(&format!("current entry digest: {}\n", asset.entry_digest));
        }
        if let Some(grant) = &status.grant {
            text.push_str(&format!("grant: {}\n", grant.grant_id));
            text.push_str(&format!("granted digest: {}\n", grant.digest));
            text.push_str(&format!(
                "granted manifest digest: {}\n",
                grant.manifest_digest
            ));
            text.push_str(&format!("granted entry digest: {}\n", grant.entry_digest));
            text.push_str(&format!("network: {}\n", grant.network));
            text.push_str(&format!(
                "grantor: {} at {} ({})\n",
                grant.grantor, grant.granted_at, grant.provenance
            ));
        }
        text.push_str(&format!("detail: {}\n", status.detail));
        Ok(Payload::detail(doc, text).into())
    }
}
