use std::path::Path;

use crate::command::{CommandOut, Payload};
use clap::Args;
use orbit_core::application::routines::resume_routine;
use serde_json::json;

#[derive(Args)]
pub struct RoutineResumeArgs {
    /// Routine name.
    pub name: String,
}

impl RoutineResumeArgs {
    pub fn execute_without_runtime(self, global_root: &Path) -> CommandOut {
        let changed = resume_routine(global_root, &self.name)?;
        let text = if changed {
            format!("resumed '{}' on this host", self.name)
        } else {
            format!("'{}' was not paused on this host", self.name)
        };
        Ok(Payload::detail(
            json!({ "routine": self.name, "paused": false, "changed": changed }),
            text,
        )
        .into())
    }
}
