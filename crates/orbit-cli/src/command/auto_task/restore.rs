use clap::Args;
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute, Payload};

use super::output::definition_to_json;

/// Reinstate a deleted shipped default with its shipped content.
///
/// Clears the opt-out `orbit auto-task delete` recorded, so later reseeds
/// manage the definition again. The restored definition is disabled, as
/// shipped.
#[derive(Args)]
pub struct AutoTaskRestoreArgs {
    /// Shipped default name
    pub name: String,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for AutoTaskRestoreArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let definition = runtime.auto_task_restore(&self.name)?;
        Ok(Payload::detail(
            definition_to_json(&definition),
            format!("{} restored from the shipped default", definition.name),
        )
        .into())
    }
}
