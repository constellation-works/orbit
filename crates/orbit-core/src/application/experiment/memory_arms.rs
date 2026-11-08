//! Which experiments and tasks carry the crew memory surface: the gate of
//! the dreaming routine, the reach of `orbit.memory.show`, and the arm
//! whose readout cost carries the dreaming overhead (crew memory §6).

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_store::contracts::{ExperimentRecord, ExperimentStoreBackend as _};

use super::definition::ExperimentOverlay;
use crate::OrbitRuntime;

/// The names of `record`'s arms whose overlay delivers crew memory.
pub(crate) fn memory_arm_names(record: &ExperimentRecord) -> Result<BTreeSet<String>, OrbitError> {
    let mut arms = BTreeSet::new();
    for arm in &record.arms {
        let overlay = ExperimentOverlay::parse(
            &arm.name,
            serde_json::from_str(&arm.overlay_json).map_err(|error| {
                OrbitError::Store(format!("invalid experiment overlay: {error}"))
            })?,
        )?;
        if overlay.memory.is_some() {
            arms.insert(arm.name.clone());
        }
    }
    Ok(arms)
}

impl OrbitRuntime {
    /// The ids of this workspace's running experiments, shadow or live,
    /// with at least one memory arm. Dreaming mints only while one exists.
    pub(crate) fn running_memory_experiments(&self) -> Result<Vec<String>, OrbitError> {
        let records = self
            .sqlite_store()?
            .experiment_records(&self.workspace_id()?, Some("running"))?;
        let mut ids = Vec::new();
        for record in &records {
            if matches!(record.experiment.mode.as_str(), "shadow" | "live")
                && !memory_arm_names(record)?.is_empty()
            {
                ids.push(record.experiment.id.clone());
            }
        }
        Ok(ids)
    }

    /// Whether `task_id` is assigned to a memory arm of any experiment.
    pub(crate) fn task_in_memory_arm(&self, task_id: &str) -> Result<bool, OrbitError> {
        let store = self.sqlite_store()?;
        let workspace = self.workspace_id()?;
        for assignment in self.task_experiments(task_id)? {
            let Some(arm) = assignment.arm else {
                continue;
            };
            let Some(record) = store.experiment_record(&workspace, &assignment.experiment)? else {
                continue;
            };
            if memory_arm_names(&record)?.contains(&arm) {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
