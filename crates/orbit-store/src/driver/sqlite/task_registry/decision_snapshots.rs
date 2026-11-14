//! Opportunistic, stamp-bound decision projections. Missing or undecodable
//! cache rows cost bundle reads; they never hide corruption in a bundle.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{ErrorCode, OptionalExtension, TransactionBehavior, params};

use super::partition_id::validate_partition_id;
use super::store::TaskRegistryStore;
use crate::contracts::TaskDecisionSnapshot;

impl TaskRegistryStore {
    /// Derived projections in this partition. A pre-ledger read-only
    /// registry has no decision cache; malformed payloads are cache misses.
    pub(crate) fn task_decision_snapshots(
        &self,
        partition_id: &str,
    ) -> Result<BTreeMap<String, TaskDecisionSnapshot>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        if self.pre_ledger {
            return Ok(BTreeMap::new());
        }
        let conn = self.read()?;
        let mut stmt = conn.prepare_cached(
            "SELECT s.task_id, s.snapshot FROM task_decision_snapshots s
             JOIN task_bundle_bindings b ON b.task_id = s.task_id AND b.workspace_id = s.workspace_id
             WHERE s.workspace_id = ?1",
        ).map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map([&partition_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(sqlite_store_error)?;
        let mut snapshots = BTreeMap::new();
        for row in rows {
            let (id, payload) = row.map_err(sqlite_store_error)?;
            if let Ok(snapshot) = serde_json::from_str::<TaskDecisionSnapshot>(&payload)
                && snapshot.task_id == id
            {
                snapshots.insert(id, snapshot);
            }
        }
        Ok(snapshots)
    }

    /// A previously observed request's owner, constrained to its current
    /// registration. Ambiguous owners require normal discovery instead.
    pub(crate) fn decision_request_task_id(
        &self,
        partition_id: &str,
        request_id: &str,
    ) -> Result<Option<String>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        if self.pre_ledger {
            return Ok(None);
        }
        self.read()?.query_row(
            "SELECT MIN(o.task_id) FROM task_decision_request_owners o
             JOIN task_bundle_bindings b ON b.task_id = o.task_id AND b.workspace_id = o.workspace_id
             WHERE o.workspace_id = ?1 AND o.request_id = ?2
             HAVING COUNT(*) = 1",
            params![partition_id, request_id], |row| row.get(0),
        ).optional().map_err(sqlite_store_error)
    }

    /// Record clean observations without waiting for a writer or syncing a
    /// derived cache. Registrations and owner rows are updated atomically.
    pub(crate) fn record_task_decision_snapshots(
        &self,
        partition_id: &str,
        records: &[TaskDecisionSnapshot],
    ) -> Result<bool, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        if records.is_empty() {
            return Ok(true);
        }
        if self.is_read_only()? {
            return Ok(false);
        }
        self.with_registry_cache_write(|conn| {
            let tx = match conn.transaction_with_behavior(TransactionBehavior::Immediate) {
                Ok(tx) => tx,
                Err(error) if error.sqlite_error_code() == Some(ErrorCode::DatabaseBusy) => return Ok(false),
                Err(error) => return Err(sqlite_store_error(error)),
            };
            {
                let mut upsert = tx.prepare_cached(
                    "INSERT INTO task_decision_snapshots(task_id, workspace_id, snapshot)
                     SELECT ?1, ?2, ?3 WHERE EXISTS (
                         SELECT 1 FROM task_bundle_bindings WHERE task_id = ?1 AND workspace_id = ?2
                     ) ON CONFLICT(task_id) DO UPDATE SET
                         workspace_id = excluded.workspace_id, snapshot = excluded.snapshot",
                ).map_err(sqlite_store_error)?;
                let mut clear = tx.prepare_cached(
                    "DELETE FROM task_decision_request_owners WHERE task_id = ?1 AND workspace_id = ?2",
                ).map_err(sqlite_store_error)?;
                let mut owner = tx.prepare_cached(
                    "INSERT INTO task_decision_request_owners(request_id, task_id, workspace_id)
                     VALUES (?1, ?2, ?3) ON CONFLICT(request_id, task_id) DO UPDATE SET workspace_id = excluded.workspace_id",
                ).map_err(sqlite_store_error)?;
                for record in records {
                    let payload = serde_json::to_string(record).map_err(|error| OrbitError::Store(error.to_string()))?;
                    if upsert.execute(params![record.task_id, partition_id, payload]).map_err(sqlite_store_error)? == 0 {
                        continue;
                    }
                    clear.execute(params![record.task_id, partition_id]).map_err(sqlite_store_error)?;
                    for request in &record.requests {
                        owner.execute(params![request.id, record.task_id, partition_id]).map_err(sqlite_store_error)?;
                    }
                }
            }
            tx.commit().map_err(sqlite_store_error)?;
            Ok(true)
        })
    }
}
