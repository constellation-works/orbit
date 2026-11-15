//! Selected-checkout status behind the resident-owned port.

use super::status;
use crate::OrbitRuntime;
use crate::application::resident::{RESIDENT_CYCLE_ROUTINE_TARGET, ResidentRoutineStatusPort};

impl ResidentRoutineStatusPort for OrbitRuntime {
    fn disabled_resident_cycle_routine_name(&self) -> Option<String> {
        let report = status::checkout_routine_statuses(self).ok()?;
        let canonical = |path: &std::path::Path| std::fs::canonicalize(path).ok();
        let orbit_dir = canonical(&self.paths().orbit_dir)?;
        report
            .statuses
            .iter()
            .find(|status| {
                canonical(&status.routine.source_orbit_dir).as_ref() == Some(&orbit_dir)
                    && status.routine.definition.target.as_ref_string()
                        == RESIDENT_CYCLE_ROUTINE_TARGET
                    && !status.routine.definition.enabled
            })
            .map(|status| status.routine.definition.name.clone())
    }
}
