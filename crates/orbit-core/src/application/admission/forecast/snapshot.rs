use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use orbit_types::task::{Task, TaskStatus, declares_no_footprint};

use crate::OrbitRuntime;
use crate::application::admission::backlog_exclusion::{
    expiring_backlog_batches, sort_tasks_for_automatic_dispatch, task_root_id,
};
use crate::application::admission::surface_reservation::reserves_surface;
use crate::application::admission::unknown_footprint::{FootprintWait, footprint_waits};
use crate::runtime::task::locks::{
    active_task_lock_holders, lock_context_files_for_task, lock_holder_index, task_lock_overlaps,
};

use super::model::ForecastAdmission;
use super::task_set::ForecastTaskSet;

/// FNV-1a over the canonical selectors: a stable footprint digest for
/// stored per-task predictions.
fn surface_digest(selectors: &[String]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in selectors.join("\n").bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

impl OrbitRuntime {
    /// Freeze the admission inputs for `tasks` (the forecast's initial and
    /// proposed tasks) at `now`. `workspace` is every workspace task, for
    /// root groups and the `review` locks the forecast does not model yet.
    pub(crate) fn forecast_admission(
        &self,
        mut tasks: Vec<Task>,
        workspace: &BTreeMap<String, Task>,
        now: DateTime<Utc>,
    ) -> ForecastAdmission {
        // Freeze overrides with the schedule. An unreadable history keeps
        // the conservative earliest start rather than forecasting early work.
        for task in &mut tasks {
            if let Some(at) = task.scheduled_after(now)
                && self
                    .get_task_history(&task.id)
                    .is_ok_and(|history| orbit_types::task::schedule_overridden(&history, at))
            {
                task.not_before = None;
            }
        }
        let workspace_root = self.paths().repo_root.as_path();
        let unlocked = self.unlocked_context_paths();
        let lock_modes = self.footprint_lock_modes();
        let len = tasks.len();
        let index_of: HashMap<&str, usize> = tasks
            .iter()
            .enumerate()
            .map(|(index, task)| (task.id.as_str(), index))
            .collect();

        // What each task would lock once admitted: the real holder rule,
        // applied as if every task were running.
        let as_running: Vec<Task> = tasks
            .iter()
            .map(|task| Task {
                status: TaskStatus::InProgress,
                ..task.clone()
            })
            .collect();
        let held_by = active_task_lock_holders(&as_running, workspace_root, unlocked);
        let mut declared: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut surfaces = Vec::with_capacity(len);
        for task in &tasks {
            let selectors = lock_context_files_for_task(task, workspace_root, unlocked);
            for selector in &selectors {
                declared
                    .entry(selector.clone())
                    .or_default()
                    .push(task.id.clone());
            }
            surfaces.push(selectors);
        }
        let review: Vec<&Task> = workspace
            .values()
            .filter(|task| task.status == TaskStatus::Review)
            .collect();
        let review_holders = active_task_lock_holders(review, workspace_root, unlocked);
        let rows = |holders: &BTreeMap<String, Vec<String>>| -> Vec<ForecastTaskSet> {
            let index = lock_holder_index(holders);
            tasks
                .iter()
                .enumerate()
                .map(|(own, task)| {
                    let mut row = ForecastTaskSet::with_len(len);
                    for overlap in
                        task_lock_overlaps(task, &index, workspace_root, unlocked, lock_modes)
                    {
                        if let Some(other) = index_of.get(overlap.locking_task_id.as_str())
                            && *other != own
                        {
                            row.insert(*other);
                        }
                    }
                    row
                })
                .collect()
        };
        let held = rows(&held_by);
        let surface = rows(&declared);
        let review_index = lock_holder_index(&review_holders);
        let review_lock_overlaps = tasks
            .iter()
            .map(|task| {
                !task_lock_overlaps(task, &review_index, workspace_root, unlocked, lock_modes)
                    .is_empty()
            })
            .collect();

        let mut root_ids: HashMap<String, usize> = HashMap::new();
        let root = tasks
            .iter()
            .map(|task| {
                let next = root_ids.len();
                *root_ids
                    .entry(task_root_id(task, workspace))
                    .or_insert(next)
            })
            .collect();

        let mut refs: Vec<&Task> = tasks.iter().collect();
        let expiring = expiring_backlog_batches(self, &refs);
        let limits = self.executor_host_os_limits().unwrap_or_else(|error| {
            tracing::warn!(%error, "backlog forecast: executor host limits unreadable");
            BTreeMap::new()
        });
        sort_tasks_for_automatic_dispatch(
            &mut refs,
            &expiring,
            &self.owner_dispatch_order(&limits),
        );
        let order = refs.iter().map(|task| index_of[task.id.as_str()]).collect();

        let no_footprint: Vec<bool> = tasks
            .iter()
            .map(|task| declares_no_footprint(&task.tags, &task.context_files))
            .collect();
        let waiting = tasks
            .iter()
            .filter(|task| matches!(task.status, TaskStatus::Backlog | TaskStatus::Proposed));
        let waits = footprint_waits(self, "backlog_forecast", waiting, now).unwrap_or_else(|error| {
            tracing::warn!(%error, "backlog forecast: footprint waits unreadable; no pilot wait");
            BTreeMap::new()
        });
        let footprint_deadline = tasks
            .iter()
            .map(|task| match waits.get(&task.id) {
                Some(FootprintWait::AwaitingFootprint { until }) => {
                    Some((*until - now).num_seconds().max(0) as f64 / 3600.0)
                }
                _ => None,
            })
            .collect();

        let exclusive_waits = tasks
            .iter()
            .zip(&no_footprint)
            .filter(|(_, none)| **none)
            .map(|(task, _)| (task.id.clone(), FootprintWait::Exclusive))
            .collect();
        let mut no_footprint_set = ForecastTaskSet::with_len(len);
        for (index, none) in no_footprint.iter().enumerate() {
            if *none {
                no_footprint_set.insert(index);
            }
        }
        ForecastAdmission {
            exclusive_waits,
            no_footprint_set,
            lookup: tasks
                .iter()
                .map(|task| (task.id.clone(), task.clone()))
                .collect(),
            order,
            held,
            surface,
            edits: surfaces
                .iter()
                .map(|selectors| !selectors.is_empty())
                .collect(),
            no_footprint,
            footprint_deadline,
            roots: root_ids.len(),
            root,
            reserves: tasks.iter().map(reserves_surface).collect(),
            expiring,
            forecast_at: now,
            surface_hash: surfaces
                .iter()
                .map(|selectors| surface_digest(selectors))
                .collect(),
            review_lock_overlaps,
            resource_fit: None,
            tasks,
        }
    }
}
