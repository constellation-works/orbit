//! Notice writes share the transaction that changes the holder. Reads are
//! bounded and ordered by the durable sequence, never by a wall clock.

use orbit_common::OrbitError;
use orbit_common::fs::selector::overlaps;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{TransactionBehavior, params};
use serde_json::{Value, json};

use super::rows::{LEASE_ROWS, parse_string_list, reservation_scope_clause};
use crate::contracts::{LeaseNotice, TaskReservationScope};
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
        reservation_scope_clause(TaskReservationScope::Files),
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
    /// Read up to `limit` notices strictly after `after`, optionally for one
    /// lease. Reopening the store preserves the sequence and all events.
    pub fn list_lease_notices(
        &self,
        orbit_dir: &str,
        workspace_id: Option<&str>,
        lease_id: Option<&str>,
        after: u64,
        limit: u32,
    ) -> Result<Vec<LeaseNotice>, OrbitError> {
        self.with_read_connection(|conn| {
            let mut stmt = conn.prepare(
                "SELECT seq, lease_id, kind, payload_json FROM lease_notices
                 WHERE ((?1 IS NOT NULL AND (workspace_id = ?1 OR (workspace_id IS NULL AND workspace_orbit_dir = ?2)))
                    OR (?1 IS NULL AND workspace_orbit_dir = ?2))
                 AND seq > ?3 AND (?4 IS NULL OR lease_id = ?4) ORDER BY seq LIMIT ?5"
            ).map_err(sqlite_store_error)?;
            let rows = stmt.query_map(params![workspace_id, orbit_dir, after, lease_id, limit], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get::<_, String>(3)?))
            }).map_err(sqlite_store_error)?;
            rows.map(|row| {
                let (seq, lease_id, kind, payload) = row.map_err(sqlite_store_error)?;
                Ok(LeaseNotice { seq, lease_id, kind, payload: serde_json::from_str(&payload)
                    .map_err(|e| OrbitError::Store(format!("invalid lease notice payload: {e}")))? })
            }).collect()
        })
    }

    /// Idempotently record overlaps discovered by landed facts or a task
    /// envelope publication. Only active leases receive them.
    pub(crate) fn record_lease_overlaps(
        &self,
        event: &LeaseOverlapNotice<'_>,
    ) -> Result<(), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            append_lease_overlaps(tx, event)
        })
    }

    /// Record a landed commit on a lease's base. Repeated analytics scans
    /// cannot duplicate a notice, and old landings cannot notify newer leases.
    pub fn record_landed_lease_notices(
        &self,
        landing: &crate::contracts::LeaseLandedNoticeParams,
    ) -> Result<(), OrbitError> {
        self.record_lease_overlaps(&LeaseOverlapNotice {
            orbit_dir: &landing.workspace_orbit_dir,
            workspace_id: Some(&landing.workspace_id),
            holder: &landing.task_id,
            paths: &landing.paths,
            kind: "overlap_landed",
            payload: json!({"commit": landing.commit, "task_id": landing.task_id, "base_branch": landing.base_branch}),
            event_key: &format!("landed:{}", landing.commit),
            landing: Some((&landing.base_branch, &landing.landed_at)),
        })
    }
}
