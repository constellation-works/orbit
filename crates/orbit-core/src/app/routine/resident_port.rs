//! Selected-checkout status behind the resident-owned port.

use super::status;
use crate::OrbitRuntime;
use crate::application::resident::RESIDENT_CYCLE_ROUTINE_TARGET;

pub(crate) struct ResidentRoutineStatus<'a>(pub(crate) &'a OrbitRuntime);

impl ResidentRoutineStatus<'_> {
    pub(crate) fn disabled_cycle_routine_name(&self) -> Option<String> {
        let rt = self.0;
        let report = status::checkout_routine_statuses(rt).ok()?;
        let canonical = |path: &std::path::Path| std::fs::canonicalize(path).ok();
        let orbit_dir = canonical(&rt.paths().orbit_dir)?;
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
