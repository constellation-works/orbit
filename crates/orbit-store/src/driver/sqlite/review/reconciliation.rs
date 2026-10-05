//! Review reconciliation records: one row per reconciliation, unique per task
//! and operator request key, replaced under a revision fence.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::ReviewReconciliation;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::ledger::{decode, encode};
use crate::Store;

fn read(
    conn: &Connection,
    workspace_id: &str,
    reconciliation_id: &str,
) -> Result<Option<ReviewReconciliation>, OrbitError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT record_json FROM review_reconciliations WHERE workspace_id=?1 AND reconciliation_id=?2",
            params![workspace_id, reconciliation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    raw.as_deref().map(decode).transpose()
}

impl Store {
    pub(super) fn reconciliation_open(
        &self,
        workspace_id: &str,
        record: &ReviewReconciliation,
    ) -> Result<ReviewReconciliation, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let existing: Option<String> = conn
                .query_row(
                    "SELECT record_json FROM review_reconciliations WHERE workspace_id=?1 AND task_id=?2 AND request_key=?3",
                    params![workspace_id, record.binding.task_id, record.request_key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if let Some(raw) = existing {
                let existing: ReviewReconciliation = decode(&raw)?;
                if existing.binding_digest != record.binding_digest {
                    return Err(OrbitError::InvalidInput(format!(
                        "review reconciliation request '{}' was already used for a different \
                         merged head or execution; choose a new request key",
                        record.request_key
                    )));
                }
                return Ok(existing);
            }
            let mut inserted = record.clone();
            inserted.revision = 1;
            inserted.updated_at = Utc::now();
            conn.execute(
                "INSERT INTO review_reconciliations (workspace_id, reconciliation_id, task_id, request_key, revision, record_json, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    workspace_id,
                    inserted.reconciliation_id,
                    inserted.binding.task_id,
                    inserted.request_key,
                    inserted.revision,
                    encode(&inserted)?,
                    inserted.updated_at.to_rfc3339(),
                ],
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(inserted)
        })
    }

    pub(super) fn reconciliation(
        &self,
        workspace_id: &str,
        reconciliation_id: &str,
    ) -> Result<Option<ReviewReconciliation>, OrbitError> {
        self.with_read_connection(|conn| read(conn, workspace_id, reconciliation_id))
    }

    pub(super) fn reconciliations_for_task(
        &self,
        workspace_id: &str,
        task_id: &str,
    ) -> Result<Vec<ReviewReconciliation>, OrbitError> {
        self.with_read_connection(|conn| {
            let mut statement = conn
                .prepare(
                    "SELECT record_json FROM review_reconciliations WHERE workspace_id=?1 AND task_id=?2 ORDER BY updated_at DESC, reconciliation_id DESC",
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            let rows = statement
                .query_map(params![workspace_id, task_id], |row| row.get::<_, String>(0))
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            rows.map(|raw| {
                raw.map_err(|error| OrbitError::Store(error.to_string()))
                    .and_then(|raw| decode(&raw))
            })
            .collect()
        })
    }

    pub(super) fn reconciliation_update(
        &self,
        workspace_id: &str,
        record: &ReviewReconciliation,
    ) -> Result<ReviewReconciliation, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let current = read(conn, workspace_id, &record.reconciliation_id)?.ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "review reconciliation '{}' does not exist",
                    record.reconciliation_id
                ))
            })?;
            if current.binding_digest != record.binding_digest
                || current.request_key != record.request_key
                || current.contract != record.contract
            {
                return Err(OrbitError::InvalidInput(
                    "a review reconciliation's binding, contract and request key are immutable"
                        .into(),
                ));
            }
            let mut updated = record.clone();
            updated.revision = record.revision.saturating_add(1);
            updated.updated_at = Utc::now();
            let changed = conn
                .execute(
                    "UPDATE review_reconciliations SET revision=?1, record_json=?2, updated_at=?3 WHERE workspace_id=?4 AND reconciliation_id=?5 AND revision=?6",
                    params![
                        updated.revision,
                        encode(&updated)?,
                        updated.updated_at.to_rfc3339(),
                        workspace_id,
                        updated.reconciliation_id,
                        record.revision,
                    ],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if changed == 0 {
                return Err(OrbitError::Store(
                    "review reconciliation changed concurrently; reread and retry".into(),
                ));
            }
            Ok(updated)
        })
    }
}
