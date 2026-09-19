//! Evidence-backed repair of task context declarations.
//!
//! Filesystem-existence pruning used to delete a declared selector as soon as
//! its target stopped existing, recording what it dropped in a
//! [`CONTEXT_FILES_PRUNED_EVENT`] history entry. Pruning is retired
//! ([ORB-12490]), but the tasks it already narrowed still carry the shrunken
//! declaration, and admission now treats a declaration as the task's real
//! footprint.
//!
//! Restoration reads that history and re-declares exactly the selectors it
//! names. There is no inference: a task whose pruning entries are absent or
//! unparseable — and a task that never declared anything — is reported for
//! operator repair rather than handed a guessed or inherited scope.

use orbit_common::OrbitError;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{Task, TaskHistoryEntry};

use super::TaskRecordUpdateParams;
use super::paths::{
    CONTEXT_FILES_PRUNED_EVENT, context_files_restored_history_entry, context_workspace_root,
    pruned_selectors_from_note,
};
use crate::OrbitRuntime;
use crate::runtime::task::{DeclaredContextFiles, declared_context_files};

/// What a history-backed restoration would add to one task, or did add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFileRestoration {
    pub task_id: String,
    /// Canonical selectors this repair adds to the task's declaration, in the
    /// order their pruning entries were recorded.
    pub restored: Vec<String>,
    /// Selectors a pruning entry names that cannot be canonicalized against
    /// this workspace — a renamed or removed path, or a selector written for
    /// a different checkout. They need an operator decision, not a guess.
    pub unrestorable: Vec<String>,
    /// Whether the restoration was written.
    pub applied: bool,
}

impl ContextFileRestoration {
    /// Whether this task has pruning evidence worth acting on.
    pub fn is_empty(&self) -> bool {
        self.restored.is_empty() && self.unrestorable.is_empty()
    }
}

impl OrbitRuntime {
    /// Report the history-backed restoration available for one task without
    /// writing anything.
    pub fn plan_context_file_restore(
        &self,
        id: &str,
    ) -> Result<ContextFileRestoration, OrbitError> {
        let task = self.get_task(id)?;
        let history = self.get_task_history(id)?;
        Ok(self.compute_context_file_restore(&task, &history))
    }

    /// Re-declare the selectors this task's pruning history recorded as
    /// dropped, appending a [`context_files_restored_history_entry`] that
    /// names each one.
    ///
    /// The read of task and history happens under the same write lock as the
    /// write, so a concurrent edit cannot be overwritten by a restoration
    /// decided against an older snapshot.
    pub fn restore_pruned_context_files(
        &self,
        id: &str,
    ) -> Result<(Task, ContextFileRestoration), OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let mut outcome: Option<(Task, ContextFileRestoration)> = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let task = self.get_task(id)?;
            let history = self.get_task_history(id)?;
            let restoration = self.compute_context_file_restore(&task, &history);
            if restoration.restored.is_empty() {
                outcome = Some((task, restoration));
                return Ok(());
            }

            let mut context_files = task.context_files.clone();
            context_files.extend(restoration.restored.iter().cloned());
            let actor = self.actor().resolve_write_label(None, None)?;
            let history_entry =
                context_files_restored_history_entry(actor.as_str(), &restoration.restored);
            let updated = self.with_mutation(|| {
                let updated = self.stores().task_records().update(
                    id,
                    TaskRecordUpdateParams {
                        actor: actor.clone(),
                        context_files: Some(context_files.clone()),
                        append_history: vec![history_entry.clone()],
                        ..Default::default()
                    },
                )?;
                Ok((
                    updated.clone(),
                    OrbitEvent::TaskUpdated { id: id.to_string() },
                ))
            })?;
            outcome = Some((
                updated,
                ContextFileRestoration {
                    applied: true,
                    ..restoration
                },
            ));
            Ok(())
        })?;

        outcome.ok_or_else(|| {
            OrbitError::Execution(
                "context restore body did not run under the task lock".to_string(),
            )
        })
    }

    /// The task's declared selectors, canonicalized without dropping absent
    /// targets, plus the declarations that cannot be canonicalized at all.
    ///
    /// Operator surfaces use it to separate "declares nothing" from "declares
    /// something unusable": the two need different repairs before admission.
    pub fn declared_context_surface(&self, task: &Task) -> DeclaredContextFiles {
        let workspace_root = context_workspace_root(&self.paths().repo_root, None);
        declared_context_files(&task.context_files, &workspace_root)
    }

    fn compute_context_file_restore(
        &self,
        task: &Task,
        history: &[TaskHistoryEntry],
    ) -> ContextFileRestoration {
        let workspace_root = context_workspace_root(&self.paths().repo_root, None);
        let declared = declared_context_files(&task.context_files, &workspace_root).retained;

        let mut restored: Vec<String> = Vec::new();
        let mut unrestorable: Vec<String> = Vec::new();
        for selector in history
            .iter()
            .filter(|entry| entry.event == CONTEXT_FILES_PRUNED_EVENT)
            .filter_map(|entry| entry.note.as_deref())
            .flat_map(pruned_selectors_from_note)
        {
            let recovered =
                declared_context_files(std::slice::from_ref(&selector), &workspace_root);
            match recovered.retained.first() {
                // Already declared again — an operator or a later edit
                // restored it, and re-adding it would duplicate the entry.
                Some(canonical) if declared.contains(canonical) || restored.contains(canonical) => {
                }
                Some(canonical) => restored.push(canonical.clone()),
                None if unrestorable.contains(&selector) => {}
                None => unrestorable.push(selector),
            }
        }

        ContextFileRestoration {
            task_id: task.id.to_string(),
            restored,
            unrestorable,
            applied: false,
        }
    }
}
