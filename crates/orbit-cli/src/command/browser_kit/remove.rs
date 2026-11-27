use clap::Args;
use orbit_common::Env;
use orbit_core::app::host::KitLayout;
use orbit_core::app::host::browser_kit;
use serde_json::json;

use crate::command::{CommandOut, Payload, require_confirmation};

#[derive(Args)]
pub struct BrowserKitRemoveArgs {
    /// Confirm deleting ~/.local/chromium-deps
    #[arg(long)]
    pub confirm: bool,
}

impl BrowserKitRemoveArgs {
    pub(super) fn execute(self, layout: &KitLayout, env: &Env) -> CommandOut {
        require_confirmation(self.confirm, "removing the browser kit")?;
        let removed = browser_kit::remove(layout, env)?;
        let root = layout.root().display().to_string();
        let text = if removed {
            format!("Removed the browser kit at {root}; workers no longer receive it.")
        } else {
            format!("No browser kit at {root}.")
        };
        Ok(Payload::detail(json!({ "root": root, "removed": removed }), text).into())
    }
}
