use clap::Args;
use orbit_core::Core;
use orbit_core::app::doctor::{Command, Outcome, exec};

use crate::command::CommandOut;

#[derive(Args)]
pub struct ProvidersArgs {}

/// `orbit doctor providers`: one row per executor definition, naming the
/// provider CLI it launches, where dispatch would find that CLI, and the
/// sandbox mode the effective definition resolves to.
pub(super) fn provider_diagnostics(core: &Core) -> CommandOut {
    let Outcome::Providers(providers) = exec(core, Command::Providers)? else {
        unreachable!("a providers command answers with its executors")
    };
    Ok(crate::output::doctor::providers_payload(providers).into())
}
