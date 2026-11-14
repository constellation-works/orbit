//! One listed row per backlog task for a backlog snapshot.
//!
//! The row carries the task, its comments, its history and its artifact
//! manifest, so the pilot, baseline and provider holds do not each read the
//! bundle again. A drain pass loads these rows when it records pilot handoffs
//! and passes them in, so the snapshot does not read the bundles again.
//! Admission that did not preload records handoffs here, before recovery
//! reconciliation. Observation only reads.

use std::collections::BTreeMap;

use orbit_store::contracts::TaskRow;
use orbit_types::task::{Task, TaskStatus};

use super::super::pass_error::PassError;
use crate::OrbitRuntime;

/// Load one lightweight row for every backlog task in `task_lookup`, record
/// pilot handoffs when `admission` is set and `prepared` is absent, reconcile
/// recovery context, and cache this pass's provider holds. `task_lookup` is
/// the admission population; reconciliation updates it in place.
///
/// `prepared` is the drain pass's already-recorded rows. Ids it already
/// holds are not read again, and handoffs are not recorded a second time. A
/// backlog id that appeared after that sweep is loaded for this snapshot's
/// holds only.
pub(super) fn prepare_pass_rows(
    runtime: &OrbitRuntime,
    action: &str,
    admission: bool,
    task_lookup: &mut BTreeMap<String, Task>,
    prepared: Option<BTreeMap<String, Option<TaskRow>>>,
) -> Result<BTreeMap<String, Option<TaskRow>>, PassError> {
    let recorded_early = prepared.is_some();
    let mut rows = prepared.unwrap_or_default();
    let order = backlog_ids_created_desc(task_lookup);
    for id in &order {
        if rows.contains_key(id) {
            continue;
        }
        match runtime.get_listed_task_row(id) {
            Ok(row) => {
                rows.insert(id.clone(), row);
            }
            Err(error) if admission => {
                return Err(PassError::store(
                    action,
                    "record pilot operator handoffs",
                    error,
                ));
            }
            Err(error) => {
                tracing::warn!(task_id = %id, "could not read backlog task row: {error}");
            }
        }
    }
    if admission && !recorded_early {
        let written = runtime
            .record_pilot_handoffs_from_rows(&rows, &order)
            .map_err(|error| PassError::store(action, "record pilot operator handoffs", error))?;
        refresh_rows(runtime, action, &written, &mut rows)?;
    }
    if admission {
        let before = recovery_context_fingerprint(task_lookup);
        super::super::recovery_context::reconcile_recovery_context(runtime, task_lookup);
        let changed = before
            .into_iter()
            .filter(|(id, context_files)| {
                task_lookup
                    .get(id)
                    .is_some_and(|task| &task.context_files != context_files)
            })
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        refresh_rows(runtime, action, &changed, &mut rows)?;
    }
    runtime.remember_admission_provider_holds(rows.iter().filter_map(|(id, row)| {
        row.as_ref()
            .map(|row| (id.as_str(), &row.task, row.history.as_slice()))
    }));
    Ok(rows)
}

/// Backlog ids in the store's listing order: newest `created_at`, then id.
fn backlog_ids_created_desc(task_lookup: &BTreeMap<String, Task>) -> Vec<String> {
    let mut tasks = task_lookup
        .values()
        .filter(|task| task.status == TaskStatus::Backlog)
        .collect::<Vec<_>>();
    tasks.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    tasks.into_iter().map(|task| task.id.clone()).collect()
}

fn recovery_context_fingerprint(
    task_lookup: &BTreeMap<String, Task>,
) -> Vec<(String, Vec<String>)> {
    task_lookup
        .values()
        .filter(|task| {
            !task.status.is_terminal()
                && task.job_run_id.is_some()
                && !task.context_files.is_empty()
        })
        .map(|task| (task.id.clone(), task.context_files.clone()))
        .collect()
}

fn refresh_rows(
    runtime: &OrbitRuntime,
    action: &str,
    ids: &[String],
    rows: &mut BTreeMap<String, Option<TaskRow>>,
) -> Result<(), PassError> {
    for id in ids {
        let row = runtime
            .get_listed_task_row(id)
            .map_err(|error| PassError::store(action, "record pilot operator handoffs", error))?;
        rows.insert(id.clone(), row);
    }
    Ok(())
}
