//! Concrete repository for lease notices (`lease_notices`): the durable,
//! sequence-ordered events a lease holder polls.
//!
//! Reached only through [`Snapshot::lease_notice`] and [`Tx::lease_notice`].
//! Notices raised by a holder change are written by the task-reservation
//! transaction that makes the change; this repository reads them and records
//! the notices a landed commit raises.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, params};
use serde_json::json;

use crate::contracts::{LeaseLandedNoticeParams, LeaseNotice};
use crate::repository::task_reservation::notices::{LeaseOverlapNotice, append_lease_overlaps};
use crate::store::{Snapshot, Tx};

/// Concrete lease-notice repository scoped to a [`Snapshot`] (read-only) or a
/// [`Tx`] (read-write).
pub struct LeaseNoticeRepository<'a, Access> {
    conn: &'a Connection,
    access: &'a Access,
}

impl<'a, 's> LeaseNoticeRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            access: snapshot,
        }
    }
}

impl<'a, 'tx> LeaseNoticeRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            access: tx,
        }
    }

    /// Record the overlap notices a commit landed on a lease's base raises.
    /// Only active leases on that base created no later than the landing
    /// receive one, keyed by the commit, so a repeated scan writes nothing
    /// twice.
    pub fn put_landed(&self, landing: &LeaseLandedNoticeParams) -> Result<(), OrbitError> {
        append_lease_overlaps(
            self.access.legacy(),
            &LeaseOverlapNotice {
                orbit_dir: &landing.workspace_orbit_dir,
                workspace_id: Some(&landing.workspace_id),
                holder: &landing.task_id,
                paths: &landing.paths,
                kind: "overlap_landed",
                payload: json!({
                    "commit": landing.commit,
                    "task_id": landing.task_id,
                    "base_branch": landing.base_branch,
                }),
                event_key: &format!("landed:{}", landing.commit),
                landing: Some((&landing.base_branch, &landing.landed_at)),
            },
        )
    }
}

impl<Access> LeaseNoticeRepository<'_, Access> {
    /// Up to `limit` notices strictly after sequence `after`, in sequence
    /// order, optionally for one lease. With a `workspace_id`, rows of that
    /// workspace and unowned rows at `orbit_dir`; without one, rows at
    /// `orbit_dir`.
    pub fn list(
        &self,
        orbit_dir: &str,
        workspace_id: Option<&str>,
        lease_id: Option<&str>,
        after: u64,
        limit: u32,
    ) -> Result<Vec<LeaseNotice>, OrbitError> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, lease_id, kind, payload_json FROM lease_notices
             WHERE ((?1 IS NOT NULL AND (workspace_id = ?1 OR (workspace_id IS NULL AND workspace_orbit_dir = ?2)))
                OR (?1 IS NULL AND workspace_orbit_dir = ?2))
             AND seq > ?3 AND (?4 IS NULL OR lease_id = ?4) ORDER BY seq LIMIT ?5"
        ).map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(
                params![workspace_id, orbit_dir, after, lease_id, limit],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .map_err(sqlite_store_error)?;
        rows.map(|row| {
            let (seq, lease_id, kind, payload) = row.map_err(sqlite_store_error)?;
            Ok(LeaseNotice {
                seq,
                lease_id,
                kind,
                payload: serde_json::from_str(&payload)
                    .map_err(|e| OrbitError::Store(format!("invalid lease notice payload: {e}")))?,
            })
        })
        .collect()
    }
}
