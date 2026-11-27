use clap::{Args, Subcommand};
use orbit_common::Env;
use orbit_core::app::host::KitLayout;

use super::install::BrowserKitInstallArgs;
use super::remove::BrowserKitRemoveArgs;
use super::status::BrowserKitStatusArgs;
use super::verify::BrowserKitVerifyArgs;
use crate::command::CommandOut;

#[derive(Args)]
#[command(
    about = "Install, verify and remove the headless browser kit workers check UIs with",
    long_about = "Install, verify and remove the headless browser kit workers check UIs with\n\n\
        The kit lives at ~/.local/chromium-deps: Playwright at the version this Orbit release \
        pins, and the Chromium headless shell it launches, downloaded from Playwright's CDN. \
        Run it on the host as the user who runs workers. Once `verify` passes, every agent \
        receives PLAYWRIGHT_BROWSERS_PATH, PLAYWRIGHT_MODULE and ORBIT_BROWSER_KIT_ENV (the \
        kit's env.sh, which a check sources first). Node must already be installed."
)]
pub struct BrowserKitCommand {
    #[command(subcommand)]
    pub command: BrowserKitSubcommand,
}

#[derive(Subcommand)]
pub enum BrowserKitSubcommand {
    /// Report the kit's version, browser, libraries and what workers receive
    Status(BrowserKitStatusArgs),
    /// Install, upgrade or adopt the kit at the pinned Playwright version, then verify it
    Install(BrowserKitInstallArgs),
    /// Run the loopback screenshot smoke test and record a pass for workers
    Verify(BrowserKitVerifyArgs),
    /// Delete the kit; workers stop receiving its variables
    Remove(BrowserKitRemoveArgs),
}

impl BrowserKitCommand {
    pub fn execute_without_runtime(self, env: &Env) -> CommandOut {
        let layout = KitLayout::current_user(env)?;
        match self.command {
            BrowserKitSubcommand::Status(args) => args.execute(&layout, env),
            BrowserKitSubcommand::Install(args) => args.execute(&layout, env),
            BrowserKitSubcommand::Verify(args) => args.execute(&layout, env),
            BrowserKitSubcommand::Remove(args) => args.execute(&layout, env),
        }
    }
}
