//! Publish staged bundles, bindings and index rows with restore rollback.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::task::{TaskEnvelopeV2, task_id_prefix};

use crate::driver::file::task_bundle::read_bundle_at;
use crate::repository::task_index::{TaskRegistryStore, parse_orb_task_number};

use super::log_rollback_failure;

pub(crate) fn commit_restore(
    registry: &TaskRegistryStore,
    workspace_id: &str,
    staging: &Path,
    restored_ids: &[String],
    previous_envelopes: Vec<TaskEnvelopeV2>,
) -> Result<(), OrbitError> {
    let previous_allocator = registry.allocator_next_number()?;
    let mut guard = RestoreGuard::new(
        registry,
        workspace_id.to_owned(),
        previous_envelopes,
        previous_allocator,
    );

    for task_id in restored_ids {
        let source = staging.join(task_id);
        let destination = registry.canonical_task_bundle_path(workspace_id, task_id)?;
        fs::rename(&source, &destination)
            .map_err(|error| OrbitError::from_write_io(&destination, error))?;
        guard.published_dirs.push(destination);
    }

    // The bundles are already in place, so the whole restored set is bound in a
    // single commit rather than one WAL fsync per task.
    let bindings = restored_ids
        .iter()
        .map(|task_id| {
            registry
                .canonical_task_bundle_path(workspace_id, task_id)
                .map(|path| (task_id.clone(), path))
        })
        .collect::<Result<Vec<_>, _>>()?;
    registry.register_task_bundles(workspace_id, &bindings)?;
    guard.registered_ids.extend(restored_ids.iter().cloned());

    rebuild_workspace_index(registry, workspace_id)?;

    let local_prefix = registry.local_task_prefix()?;
    let target_allocator = restored_ids
        .iter()
        .filter_map(|task_id| local_task_number(task_id, &local_prefix))
        .max()
        .map_or(Ok(previous_allocator), |value| {
            value.checked_add(1).ok_or_else(|| {
                OrbitError::InvalidInput(
                    "publication restore: local task id exceeds maximum allocatable task id".into(),
                )
            })
        })?
        .max(previous_allocator);
    if target_allocator > previous_allocator {
        registry.bump_allocator_to_at_least(target_allocator)?;
        guard.advanced_allocator = Some(target_allocator);
    }

    guard.commit();
    Ok(())
}

fn rebuild_workspace_index(
    registry: &TaskRegistryStore,
    workspace_id: &str,
) -> Result<(), OrbitError> {
    let envelopes = registry
        .tasks_for_workspace(workspace_id)?
        .into_iter()
        .map(|binding| read_bundle_at(&binding.canonical_path).map(|bundle| bundle.envelope))
        .collect::<Result<Vec<_>, _>>()?;
    registry.replace_workspace_task_indexes(workspace_id, &envelopes)
}

struct RestoreGuard<'a> {
    registry: &'a TaskRegistryStore,
    workspace_id: String,
    previous_envelopes: Vec<orbit_types::task::TaskEnvelopeV2>,
    previous_allocator: u32,
    advanced_allocator: Option<u32>,
    published_dirs: Vec<PathBuf>,
    registered_ids: Vec<String>,
    armed: bool,
}

impl<'a> RestoreGuard<'a> {
    fn new(
        registry: &'a TaskRegistryStore,
        workspace_id: String,
        previous_envelopes: Vec<orbit_types::task::TaskEnvelopeV2>,
        previous_allocator: u32,
    ) -> Self {
        Self {
            registry,
            workspace_id,
            previous_envelopes,
            previous_allocator,
            advanced_allocator: None,
            published_dirs: Vec::new(),
            registered_ids: Vec::new(),
            armed: true,
        }
    }

    fn commit(&mut self) {
        self.armed = false;
    }

    fn rollback(&mut self) {
        for task_id in self.registered_ids.iter().rev() {
            log_rollback_failure(
                "unregister restored task",
                self.registry
                    .unregister_task_bundle(task_id, &self.workspace_id),
            );
        }
        log_rollback_failure(
            "restore workspace task indexes",
            self.registry
                .replace_workspace_task_indexes(&self.workspace_id, &self.previous_envelopes),
        );
        for dir in self.published_dirs.iter().rev() {
            log_rollback_failure("remove restored bundle", fs::remove_dir_all(dir));
        }
        if let Some(current) = self.advanced_allocator {
            log_rollback_failure(
                "restore task id allocator",
                self.registry
                    .restore_allocator_after_failed_restore(current, self.previous_allocator),
            );
        }
        self.armed = false;
    }
}

impl Drop for RestoreGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.rollback();
        }
    }
}

/// Numeric suffix of `task_id`, but only for ids this host mints. A mirror
/// carries its owner's numbering, which must not advance the local allocator.
fn local_task_number(task_id: &str, local_prefix: &str) -> Option<u32> {
    (task_id_prefix(task_id) == Some(local_prefix))
        .then(|| parse_orb_task_number(task_id))
        .flatten()
}
