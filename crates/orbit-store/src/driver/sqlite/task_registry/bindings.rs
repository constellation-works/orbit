//! Task-bundle bindings: canonical bundle paths and the task-id to
//! partition rows.

use super::partition_id::validate_partition_id;
use super::queries::{decode_task_bundle_binding, task_bundle_by_id, workspace_by_id};
use super::store::TaskRegistryStore;
use super::util::{now_string, path_to_string};
use crate::contracts::TaskBundleBinding;
use crate::fs::path_safety::normalize_path;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::validate_orb_task_id;
use rusqlite::{Connection, TransactionBehavior, params};
use std::path::{Path, PathBuf};

fn upsert_task_binding(
    tx: &Connection,
    task_id: &str,
    partition_id: &str,
    canonical_path: &Path,
    now: &str,
) -> Result<(), OrbitError> {
    tx.prepare_cached(
        "INSERT INTO task_bundle_bindings (
            task_id, workspace_id, canonical_path, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?4)
        ON CONFLICT(task_id) DO UPDATE SET
            workspace_id = excluded.workspace_id,
            canonical_path = excluded.canonical_path,
            updated_at = excluded.updated_at",
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?
    .execute(params![
        task_id,
        partition_id,
        path_to_string(canonical_path),
        now
    ])
    .map_err(|e| OrbitError::Store(e.to_string()))?;
    Ok(())
}

impl TaskRegistryStore {
    pub fn canonical_task_bundle_path(
        &self,
        partition_id: &str,
        task_id: &str,
    ) -> Result<PathBuf, OrbitError> {
        validate_orb_task_id(task_id)?;
        Ok(self.workspace_partition_dir(partition_id)?.join(task_id))
    }

    /// The directory holding one partition's task bundles. Callers that
    /// coordinate a whole partition (rather than one bundle) anchor their
    /// state here.
    pub fn workspace_partition_dir(&self, partition_id: &str) -> Result<PathBuf, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        Ok(self.workspaces_dir.join(partition_id))
    }

    pub fn register_task_bundle(
        &self,
        task_id: &str,
        partition_id: &str,
        canonical_path: &Path,
    ) -> Result<TaskBundleBinding, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let canonical_path = self.validated_binding_path(&partition_id, task_id, canonical_path)?;

        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        if workspace_by_id(&tx, &partition_id)?.is_none() {
            return Err(OrbitError::not_found(NotFoundKind::Workspace, partition_id));
        }

        upsert_task_binding(&tx, task_id, &partition_id, &canonical_path, &now_string())?;

        let binding = task_bundle_by_id(&tx, task_id)?.ok_or_else(|| {
            OrbitError::Store("failed to read inserted task bundle binding".into())
        })?;
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(binding)
    }

    /// Register every `(task_id, canonical_path)` binding in one transaction.
    ///
    /// Bulk publishers — reindex, migration import, publication restore — land
    /// a whole set at once, so they pay one `BEGIN IMMEDIATE` commit and one
    /// WAL fsync for the set instead of one per task. Each entry is validated
    /// exactly as [`register_task_bundle`](Self::register_task_bundle)
    /// validates its single binding, and the set is all-or-nothing: a rejected
    /// entry leaves every binding in the batch untouched.
    pub fn register_task_bundles(
        &self,
        partition_id: &str,
        bundles: &[(String, PathBuf)],
    ) -> Result<(), OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        if bundles.is_empty() {
            return Ok(());
        }
        let mut rows = Vec::with_capacity(bundles.len());
        for (task_id, canonical_path) in bundles {
            rows.push((
                task_id,
                self.validated_binding_path(&partition_id, task_id, canonical_path)?,
            ));
        }

        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        if workspace_by_id(&tx, &partition_id)?.is_none() {
            return Err(OrbitError::not_found(NotFoundKind::Workspace, partition_id));
        }

        let now = now_string();
        for (task_id, canonical_path) in &rows {
            upsert_task_binding(&tx, task_id, &partition_id, canonical_path, &now)?;
        }
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// A binding may only ever name the path this registry derives for its id,
    /// so both registration paths resolve and check it the same way.
    fn validated_binding_path(
        &self,
        partition_id: &str,
        task_id: &str,
        canonical_path: &Path,
    ) -> Result<PathBuf, OrbitError> {
        validate_orb_task_id(task_id)?;
        let canonical_path = normalize_path(canonical_path);
        let expected_path =
            normalize_path(&self.canonical_task_bundle_path(partition_id, task_id)?);
        if canonical_path != expected_path {
            return Err(OrbitError::InvalidInput(format!(
                "canonical path for task '{task_id}' in workspace '{partition_id}' must be '{}', got '{}'",
                expected_path.display(),
                canonical_path.display()
            )));
        }
        Ok(canonical_path)
    }

    pub fn unregister_task_bundle(
        &self,
        task_id: &str,
        partition_id: &str,
    ) -> Result<bool, OrbitError> {
        validate_orb_task_id(task_id)?;
        let partition_id = validate_partition_id(partition_id)?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let Some(binding) = task_bundle_by_id(&tx, task_id)? else {
            return Ok(false);
        };
        if binding.partition_id != partition_id {
            return Ok(false);
        }

        tx.execute(
            "DELETE FROM task_bundle_relations
             WHERE source_task_id = ?1 OR target_task_id = ?1",
            [task_id],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
        tx.execute("DELETE FROM task_bundle_tags WHERE task_id = ?1", [task_id])
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        tx.execute(
            "DELETE FROM task_bundle_index WHERE task_id = ?1",
            [task_id],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
        let deleted = tx
            .execute(
                "DELETE FROM task_bundle_bindings
                 WHERE task_id = ?1 AND workspace_id = ?2",
                params![task_id, partition_id],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(deleted > 0)
    }

    pub fn tasks_for_workspace(
        &self,
        partition_id: &str,
    ) -> Result<Vec<TaskBundleBinding>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let conn = self.read()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT task_id, workspace_id, canonical_path, created_at, updated_at
                 FROM task_bundle_bindings
                 WHERE workspace_id = ?1
                 ORDER BY task_id ASC",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map([partition_id], decode_task_bundle_binding)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Look up a task-bundle binding by task id. Task ids are a global primary
    /// key in the registry, so this reports collisions across every workspace —
    /// exactly what import conflict resolution needs.
    pub fn find_task_binding(
        &self,
        task_id: &str,
    ) -> Result<Option<TaskBundleBinding>, OrbitError> {
        validate_orb_task_id(task_id)?;
        let conn = self.read()?;
        task_bundle_by_id(&conn, task_id)
    }
}
