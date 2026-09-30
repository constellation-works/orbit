use std::path::Path;

use crate::command::{CommandOut, Payload};
use clap::Args;
use orbit_cmd::registry_routines::routine_statuses;
use orbit_core::OrbitError;
use orbit_core::application::routines::pause_routine;
use serde_json::json;

#[derive(Args)]
pub struct RoutinePauseArgs {
    /// Routine name.
    pub name: String,
}

impl RoutinePauseArgs {
    pub fn execute_without_runtime(self, global_root: &Path) -> CommandOut {
        // A pause is persisted by name, so a misspelt name would otherwise be
        // recorded and reported as success while pausing nothing.
        let report = routine_statuses(global_root)?;
        let known = report
            .statuses
            .iter()
            .any(|status| status.routine.definition.name == self.name)
            || report
                .inactive_plugin_routines()
                .any(|routine| routine.name == self.name);
        if !known {
            return Err(OrbitError::InvalidInput(format!(
                "no routine named '{}' (see `orbit routine list`)",
                self.name
            )));
        }
        let changed = pause_routine(global_root, &self.name, "human")?;
        let text = if changed {
            format!(
                "paused '{}' on this host (host-local; resume with `orbit routine resume {}`)",
                self.name, self.name
            )
        } else {
            format!("'{}' is already paused on this host", self.name)
        };
        Ok(Payload::detail(
            json!({ "routine": self.name, "paused": true, "changed": changed }),
            text,
        )
        .into())
    }
}
