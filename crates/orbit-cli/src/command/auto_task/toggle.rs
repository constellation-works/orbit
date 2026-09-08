use clap::{Args, ValueEnum};
use orbit_core::OrbitRuntime;

use crate::command::{CommandOut, Execute, Payload};

use super::output::definition_to_json;

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub enum ToggleState {
    /// Enable the definition
    On,
    /// Disable the definition (preserved, not deleted)
    Off,
}

#[derive(Args)]
pub struct AutoTaskToggleArgs {
    /// Definition name
    pub name: String,
    /// Whether to enable (`on`) or disable (`off`)
    #[arg(value_enum)]
    pub state: ToggleState,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for AutoTaskToggleArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        let enabled = self.state == ToggleState::On;
        let definition = runtime.auto_task_toggle(&self.name, enabled)?;

        let status = if definition.enabled {
            "enabled"
        } else {
            "disabled"
        };
        Ok(Payload::detail(
            definition_to_json(&definition),
            format!("{} {status}", definition.name),
        )
        .into())
    }
}
