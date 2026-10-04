//! Widening a task's declaration to cover paths its agents changed.
//!
//! Implementers, recovery agents and reviewers may change any path the work
//! requires. Delivery accepts the change and appends an exact `file:`
//! selector for every path the task's selectors did not cover, recording the
//! step that introduced it in a [`CONTEXT_FILES_WIDENED_EVENT`] history entry.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::selector::overlaps;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, ContextWideningStep, TaskHistoryEntry,
};

use super::{SYSTEM_ACTOR_LABEL, TaskRecordUpdateParams};
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Append `file:<path>` to `task_id` for each of `paths` no selector of
    /// the task covers, with one history entry naming `run_id`, `step` and
    /// `activity`. Returns the selectors appended; nothing is written when
    /// every path is already covered.
    ///
    /// The read and write share the task write lock, so a concurrent edit is
    /// never overwritten by a widening decided against an older snapshot.
    pub(crate) fn widen_context_files_for_paths(
        &self,
        task_id: &str,
        run_id: &str,
        step: ContextWideningStep,
        activity: &str,
        paths: &[String],
    ) -> Result<Vec<String>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let mut added = Vec::new();
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let task = self.get_task(task_id)?;
                added = uncovered_selectors(&task.context_files, paths);
                if added.is_empty() {
                    return Ok(());
                }
                let note = serde_json::to_string(&ContextFilesWidening {
                    run_id: run_id.to_string(),
                    step,
                    activity: activity.to_string(),
                    selectors: added.clone(),
                })
                .map_err(|error| OrbitError::Execution(format!("encode widening note: {error}")))?;
                let mut context_files = task.context_files.clone();
                context_files.extend(added.iter().cloned());
                self.with_mutation(|| {
                    let updated = self.stores().task_records().update(
                        task_id,
                        TaskRecordUpdateParams {
                            actor: SYSTEM_ACTOR_LABEL.to_string(),
                            context_files: Some(context_files.clone()),
                            append_history: vec![TaskHistoryEntry {
                                at: Utc::now(),
                                by: SYSTEM_ACTOR_LABEL.to_string(),
                                event: CONTEXT_FILES_WIDENED_EVENT.to_string(),
                                note: Some(note.clone()),
                                from_status: None,
                                to_status: None,
                            }],
                            ..Default::default()
                        },
                    )?;
                    Ok((
                        updated,
                        OrbitEvent::TaskUpdated {
                            id: task_id.to_string(),
                        },
                    ))
                })?;
                Ok(())
            })?;
        Ok(added)
    }
}

/// One deduplicated `file:` selector per repository-relative path that no
/// existing selector covers, in input order.
fn uncovered_selectors(context_files: &[String], paths: &[String]) -> Vec<String> {
    let mut added: Vec<String> = Vec::new();
    for path in paths {
        let path = path.trim().trim_start_matches("./");
        if path.is_empty() {
            continue;
        }
        let selector = format!("file:{path}");
        if added.contains(&selector)
            || context_files
                .iter()
                .any(|existing| overlaps(existing, &selector))
        {
            continue;
        }
        added.push(selector);
    }
    added
}
