//! Which experiments and tasks carry crew memory: the one switch of the
//! whole dormant machinery (a running experiment with a memory arm), the
//! reach of `orbit.memory.show`, and the arm whose readout cost carries the
//! dreaming overhead (crew memory §6).

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_store::contract::ExperimentRecord;

use super::definition::ExperimentOverlay;
use crate::OrbitRuntime;

/// The ids of this workspace's running experiments, shadow or live,
/// with at least one memory arm. Dreaming mints only while one exists.
pub(crate) fn running_memory_experiments(rt: &OrbitRuntime) -> Result<Vec<String>, OrbitError> {
    let records = rt
        .sqlite_store()?
        .read()?
        .experiment()
        .list(&rt.workspace_id()?, Some("running"))?;
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

/// Whether crew memory is switched on in this workspace: a running
/// shadow or live experiment has a memory arm. It gates every memory
/// write, delivery, capture and dreaming pass, read from the store each
/// time so concluding the experiment stops them at once. A store that
/// cannot be read keeps crew memory off.
pub fn crew_memory_experiment_running(rt: &OrbitRuntime) -> bool {
    match running_memory_experiments(rt) {
        Ok(ids) => !ids.is_empty(),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "crew memory stays off: running memory experiments could not be read"
            );
            false
        }
    }
}

/// Whether `task_id` is assigned to a memory arm of any experiment.
pub(crate) fn task_in_memory_arm(rt: &OrbitRuntime, task_id: &str) -> Result<bool, OrbitError> {
    let store = rt.sqlite_store()?;
    let workspace = rt.workspace_id()?;
    for assignment in super::assignment::task_experiments(rt, task_id)? {
        let Some(arm) = assignment.arm else {
            continue;
        };
        let Some(record) = store
            .read()?
            .experiment()
            .get(&workspace, &assignment.experiment)?
        else {
            continue;
        };
        if memory_arm_names(&record)?.contains(&arm) {
            return Ok(true);
        }
    }
    Ok(false)
}

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
