use clap::Args;
use orbit_common::Env;
use orbit_core::app::host::KitLayout;
use orbit_core::app::host::browser_kit;

use super::support::{to_json, verify_text};
use crate::command::{CommandOut, Payload};

#[derive(Args)]
pub struct BrowserKitVerifyArgs {}

impl BrowserKitVerifyArgs {
    pub(super) fn execute(self, layout: &KitLayout, env: &Env) -> CommandOut {
        let report = browser_kit::verify(layout, env)?;
        let exit_code = i32::from(!report.passed);
        Ok(Payload::detail(to_json(&report)?, verify_text(&report))
            .with_exit_code(exit_code)
            .into())
    }
}
