//! Notice writes share the transaction that changes the holder. Reads are
//! bounded and ordered by the durable sequence, never by a wall clock.

use orbit_common::OrbitError;
use orbit_common::fs::selector::overlaps;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{TransactionBehavior, params};
use serde_json::{Value, json};

use super::rows::{LEASE_ROWS, parse_string_list, reservation_scope_clause};
use crate::contract::TaskReservationScope;
use crate::{Store, StoreTx};

pub(super) fn append_lease_notice(
    tx: &StoreTx<'_>,
    lease_id: &str,
    kind: &str,
    payload: &Value,
    event_key: &str,
) -> Result<(), OrbitError> {
    tx.tx.execute(
        "INSERT INTO lease_notices(workspace_id, workspace_orbit_dir, lease_id, kind, payload_json, event_key)
         SELECT workspace_id, workspace_orbit_dir, reservation_id, ?2, ?3, ?4
         FROM task_reservations WHERE reservation_id = ?1 AND kind = 'lease'
         ON CONFLICT(lease_id, event_key) DO NOTHING",
        params![lease_id, kind, payload.to_string(), event_key],
    ).map_err(sqlite_store_error)?;
    Ok(())
}

/// A holder's new surface. A landing additionally binds its base and time.
pub(crate) struct LeaseOverlapNotice<'a> {
    pub(crate) orbit_dir: &'a str,
    pub(crate) workspace_id: Option<&'a str>,
    pub(crate) holder: &'a str,
    pub(crate) paths: &'a [String],
    pub(crate) kind: &'a str,
    pub(crate) payload: Value,
    pub(crate) event_key: &'a str,
    pub(crate) landing: Option<(&'a str, &'a str)>,
}

pub(crate) fn append_lease_overlaps(
    tx: &StoreTx<'_>,
    event: &LeaseOverlapNotice<'_>,
) -> Result<(), OrbitError> {
    let now = crate::now_string();
    let sql = format!(
        "SELECT reservation_id, files_json, base_branch, created_at FROM task_reservations
         WHERE {} AND {LEASE_ROWS} AND released_at IS NULL AND expires_at > ?3
         AND reservation_id != ?4 ORDER BY reservation_id",
        reservation_scope_clause(TaskReservationScope::Files, event.workspace_id),
    );
    let mut stmt = tx.tx.prepare(&sql).map_err(sqlite_store_error)?;
    let rows = stmt
        .query_map(
            params![event.workspace_id, event.orbit_dir, now, event.holder],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(sqlite_store_error)?;
    for row in rows {
        let (lease_id, files, base, created_at) = row.map_err(sqlite_store_error)?;
        if let Some((branch, at)) = event.landing
            && (base.as_deref() != Some(branch)
                || chrono::DateTime::parse_from_rfc3339(&created_at)
                    .ok()
                    .zip(chrono::DateTime::parse_from_rfc3339(at).ok())
                    .is_none_or(|(created, landed)| created > landed))
        {
            continue;
        }
        let selectors = parse_string_list(&files)?;
        let paths: Vec<_> = event
            .paths
            .iter()
            .filter(|path| selectors.iter().any(|selector| overlaps(path, selector)))
            .collect();
        if paths.is_empty() {
            continue;
        }
        let mut payload = event.payload.clone();
        payload["paths"] = json!(paths);
        append_lease_notice(tx, &lease_id, event.kind, &payload, event.event_key)?;
    }
    Ok(())
}

impl Store {
    /// Idempotently record overlaps a task envelope publication discovered.
    /// Only active leases receive them.
    pub(crate) fn record_lease_overlaps(
        &self,
        event: &LeaseOverlapNotice<'_>,
    ) -> Result<(), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            append_lease_overlaps(tx, event)
        })
    }
}
