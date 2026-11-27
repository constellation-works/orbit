use clap::Args;
use orbit_common::Env;
use orbit_core::app::host::browser_kit;
use orbit_core::bootstrap::browser_kit::KitLayout;

use super::support::{status_json, status_text};
use crate::command::{CommandOut, Payload};

#[derive(Args)]
pub struct BrowserKitStatusArgs {}

impl BrowserKitStatusArgs {
    pub(super) fn execute(self, layout: &KitLayout, env: &Env) -> CommandOut {
        let status = browser_kit::kit_status(layout, env);
        Ok(Payload::detail(status_json(&status)?, status_text(&status)).into())
    }
}
