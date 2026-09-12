use std::path::Path;

use crate::command::{CommandOut, CommandOutput};
use clap::Args;
use orbit_automation::routines::pause_routine;

#[derive(Args)]
pub struct RoutinePauseArgs {
    /// Routine name.
    pub name: String,
}

impl RoutinePauseArgs {
    pub fn execute_without_runtime(self, global_root: &Path) -> CommandOut {
        if pause_routine(global_root, &self.name, "human")? {
            println!(
                "paused '{}' on this host (host-local; resume with `orbit routine resume {}`)",
                self.name, self.name
            );
        } else {
            println!("'{}' is already paused on this host", self.name);
        }
        Ok(CommandOutput::Silent)
    }
}
