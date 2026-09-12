use std::path::Path;

use crate::command::{CommandOut, CommandOutput};
use clap::Args;
use orbit_automation::routines::resume_routine;

#[derive(Args)]
pub struct RoutineResumeArgs {
    /// Routine name.
    pub name: String,
}

impl RoutineResumeArgs {
    pub fn execute_without_runtime(self, global_root: &Path) -> CommandOut {
        if resume_routine(global_root, &self.name)? {
            println!("resumed '{}' on this host", self.name);
        } else {
            println!("'{}' was not paused on this host", self.name);
        }
        Ok(CommandOutput::Silent)
    }
}
