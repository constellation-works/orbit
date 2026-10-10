//! Atomic task-pilot reservations.
//!
//! A pilot run's tasks used to become visible to other runs only through its
//! successful prepare checkpoint, which persists after prepare returns. Two
//! runs preparing one task inside that window both selected it, and both did
//! the agent work. Prepare therefore reserves its selection inside the
//! workspace's commit boundary: the check against every live holder and the
//! write of this run's reservation are one critical section, so exactly one
//! run keeps each task. A reservation lasts until its run is terminal.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, open_read_only_no_follow};
use serde::{Deserialize, Serialize};

use crate::OrbitRuntime;

use super::preparation::active_task_pilot_preparations;

const PILOT_JOB: &str = "task_pilot_pipeline";
const RESERVATIONS_FILE: &str = "task-pilot-reservations.json";
const SCHEMA_VERSION: u32 = 1;

#[derive(Default, Serialize, Deserialize)]
struct Reservations {
    #[serde(default)]
    schema_version: u32,
    /// The task IDs each pilot run reserved, by run ID.
    #[serde(default)]
    runs: BTreeMap<String, BTreeSet<String>>,
}

/// Tasks a live pilot run has reserved, with the reserving run IDs.
pub(crate) fn live_reservations(
    runtime: &OrbitRuntime,
) -> Result<BTreeMap<String, BTreeSet<String>>, OrbitError> {
    let mut by_task = BTreeMap::<String, BTreeSet<String>>::new();
    for (run_id, task_ids) in read(runtime)?.runs {
        if !run_is_live(runtime, &run_id)? {
            continue;
        }
        for task_id in task_ids {
            by_task.entry(task_id).or_default().insert(run_id.clone());
        }
    }
    Ok(by_task)
}

/// Reserve `task_ids` for pilot run `run_id`, and return those another live
/// pilot run already holds, by reservation or prepare checkpoint, with their
/// holders. Held tasks stay with their holder; the rest replace whatever this
/// run reserved before, so a retried prepare keeps its own tasks. Without a
/// run ID nothing is reserved, but holders are still reported.
pub(crate) fn reserve(
    runtime: &OrbitRuntime,
    run_id: Option<&str>,
    task_ids: &[String],
) -> Result<BTreeMap<String, BTreeSet<String>>, OrbitError> {
    if task_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    runtime.admission_boundary()?.with_admission(|| {
        let mut held = active_task_pilot_preparations(runtime)?;
        held.retain(|task_id, holders| {
            if let Some(run_id) = run_id {
                holders.remove(run_id);
            }
            !holders.is_empty() && task_ids.contains(task_id)
        });
        let Some(run_id) = run_id else {
            return Ok(held);
        };
        let mut reservations = Reservations {
            schema_version: SCHEMA_VERSION,
            runs: BTreeMap::new(),
        };
        for (holder, tasks) in read(runtime)?.runs {
            if holder != run_id && run_is_live(runtime, &holder)? {
                reservations.runs.insert(holder, tasks);
            }
        }
        let reserved = task_ids
            .iter()
            .filter(|task_id| !held.contains_key(*task_id))
            .cloned()
            .collect::<BTreeSet<_>>();
        if !reserved.is_empty() {
            reservations.runs.insert(run_id.to_string(), reserved);
        }
        write(runtime, &reservations)?;
        Ok(held)
    })
}

fn run_is_live(runtime: &OrbitRuntime, run_id: &str) -> Result<bool, OrbitError> {
    match runtime.show_job_run(run_id) {
        Ok(run) => Ok(run.job_id == PILOT_JOB && !run.state.is_terminal()),
        Err(OrbitError::NotFound { .. }) => Ok(false),
        Err(error) => Err(error),
    }
}

fn path(runtime: &OrbitRuntime) -> PathBuf {
    runtime.paths().state_dir.join(RESERVATIONS_FILE)
}

fn read(runtime: &OrbitRuntime) -> Result<Reservations, OrbitError> {
    let path = path(runtime);
    let mut text = String::new();
    match open_read_only_no_follow(&path).and_then(|mut file| file.read_to_string(&mut text)) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Reservations::default());
        }
        Err(error) => return Err(error.into()),
    }
    // A reservation only covers the window before its run's prepare
    // checkpoint holds the same tasks, so an unreadable file is dropped
    // rather than stopping every pilot; the next reservation rewrites it.
    Ok(serde_json::from_str(&text).unwrap_or_else(|error| {
        tracing::warn!(
            path = %path.display(),
            error = %error,
            "ignoring unreadable task-pilot reservations"
        );
        Reservations::default()
    }))
}

fn write(runtime: &OrbitRuntime, reservations: &Reservations) -> Result<(), OrbitError> {
    let path = path(runtime);
    let text = serde_json::to_string_pretty(reservations).map_err(|error| {
        OrbitError::Store(format!("serialize task-pilot reservations: {error}"))
    })?;
    atomic_write_text(&path, &text).map_err(|error| OrbitError::from_write_io(&path, error))
}
