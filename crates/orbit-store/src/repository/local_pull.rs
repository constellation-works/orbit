//! Concrete repository for local pull admissions (`local_pull_admissions`):
//! the durable record of each request a pull drain sent an owner, its
//! receipt, its leaf run and its settlement.
//!
//! Reached only through [`Snapshot::local_pull`] and [`Tx::local_pull`]. The
//! admission rules (capacity, the phase machine, leaf creation, the failure
//! breaker's streak) belong to the calling operation; this repository owns
//! SQL and record decoding only. Where SQL narrows a read on JSON fields, the
//! decoded record stays the authority and is re-checked here.

use std::marker::PhantomData;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, OptionalExtension, params};

use crate::contract::{LocalPullAdmission, PullDestination};
use crate::store::{Snapshot, Tx};

/// Serialized names (`LocalPullPhase` is `snake_case`) of the phases that no
/// longer hold a drain slot: an idle poll, a settled claim, a refused request.
/// SQL narrows reads to the rest so an occupancy check inside the writer
/// transaction, and every settle-only pass, never decodes finished history.
/// [`LocalPullAdmission::holds_capacity`] stays the authority: rows are
/// re-filtered by it, so a phase missing here is read too much, never lost.
const RELEASED_PHASES_SQL: &str = "('idle','settled','refused')";

/// Concrete local-pull repository scoped to a [`Snapshot`] (read-only) or a
/// [`Tx`] (read-write).
pub struct LocalPullRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> LocalPullRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> LocalPullRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Insert or replace one admission, keyed by its destination and request
    /// id.
    pub fn put(&self, workspace_id: &str, record: &LocalPullAdmission) -> Result<(), OrbitError> {
        let claim = record
            .receipt
            .as_ref()
            .and_then(|r| r.claim.as_ref())
            .map(|c| c.claim_id.as_str());
        let record_json =
            serde_json::to_string(record).map_err(|error| OrbitError::Store(error.to_string()))?;
        self.conn
            .execute(
                "INSERT INTO local_pull_admissions VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
        ON CONFLICT(workspace_id,owner_machine,owner_workspace,execution_machine,request_id)
        DO UPDATE SET claim_id=excluded.claim_id, leaf_run_id=excluded.leaf_run_id, record_json=excluded.record_json",
                params![
                    workspace_id,
                    record.destination.owner_machine_id,
                    record.destination.owner_workspace_id,
                    record.destination.execution_machine_id,
                    record.request.request_id,
                    claim,
                    record.leaf_run_id,
                    record_json
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }

    /// Drop all but the newest `retained` `Idle` and `Refused` rows of
    /// `workspace_id`, oldest first by insertion order. `Settled` rows are
    /// never deleted here.
    pub fn delete_terminal_beyond(
        &self,
        workspace_id: &str,
        retained: usize,
    ) -> Result<(), OrbitError> {
        // A cheap count keeps the common pass from scanning JSON at all.
        let rows: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM local_pull_admissions WHERE workspace_id=?1",
                [workspace_id],
                |row| row.get(0),
            )
            .map_err(sqlite_store_error)?;
        let retained = i64::try_from(retained).unwrap_or(i64::MAX);
        if rows <= retained {
            return Ok(());
        }
        self.conn
            .execute(
                "DELETE FROM local_pull_admissions WHERE workspace_id=?1 \
                 AND json_extract(record_json,'$.phase') IN ('idle','refused') \
                 AND rowid NOT IN (SELECT rowid FROM local_pull_admissions WHERE workspace_id=?1 \
                     AND json_extract(record_json,'$.phase') IN ('idle','refused') \
                     ORDER BY rowid DESC LIMIT ?2)",
                params![workspace_id, retained],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }
}

impl<Access> LocalPullRepository<'_, Access> {
    /// One admission by destination and request id.
    pub fn get(
        &self,
        workspace_id: &str,
        destination: &PullDestination,
        request_id: &str,
    ) -> Result<Option<LocalPullAdmission>, OrbitError> {
        self.conn
            .query_row(
                "SELECT record_json FROM local_pull_admissions WHERE workspace_id=?1 \
                 AND owner_machine=?2 AND owner_workspace=?3 AND execution_machine=?4 \
                 AND request_id=?5",
                params![
                    workspace_id,
                    destination.owner_machine_id,
                    destination.owner_workspace_id,
                    destination.execution_machine_id,
                    request_id
                ],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(sqlite_store_error)?
            .map(decode)
            .transpose()
    }

    /// The admission bound to leaf run `run_id`, through the indexed column,
    /// so a reader never scans historical receipts to learn a run is
    /// unclaimed.
    pub fn get_by_leaf(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Option<LocalPullAdmission>, OrbitError> {
        self.conn
            .query_row(
                "SELECT record_json FROM local_pull_admissions WHERE workspace_id=?1 AND leaf_run_id=?2",
                params![workspace_id, run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(sqlite_store_error)?
            .map(decode)
            .transpose()
    }

    /// Every admission of `workspace_id`, in admission order.
    pub fn list(&self, workspace_id: &str) -> Result<Vec<LocalPullAdmission>, OrbitError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT record_json FROM local_pull_admissions WHERE workspace_id=?1 ORDER BY rowid",
            )
            .map_err(sqlite_store_error)?;
        let raw = stmt
            .query_map([workspace_id], |r| r.get::<_, String>(0))
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)?;
        raw.into_iter().map(decode).collect()
    }

    /// Admissions that still hold a slot — not idle, refused or settled — in
    /// admission order. Finished history is not decoded.
    pub fn list_holding(&self, workspace_id: &str) -> Result<Vec<LocalPullAdmission>, OrbitError> {
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT record_json FROM local_pull_admissions WHERE workspace_id=?1 \
                 AND json_extract(record_json,'$.phase') NOT IN {RELEASED_PHASES_SQL} ORDER BY rowid"
            ))
            .map_err(sqlite_store_error)?;
        let raw = stmt
            .query_map([workspace_id], |r| r.get::<_, String>(0))
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)?;
        let mut holding = Vec::with_capacity(raw.len());
        for record in raw.into_iter().map(decode) {
            let record = record?;
            if record.holds_capacity() {
                holding.push(record);
            }
        }
        Ok(holding)
    }

    /// Leaf run ids of settled admissions whose record does not yet say their
    /// build output is reclaimed, in admission order.
    ///
    /// `leaf_run_id IS NOT NULL` is the cheap column test that skips idle polls
    /// and refused requests without parsing their JSON: only a claim that
    /// created a leaf has one. The phase and the reclaim marker live in the
    /// record, so the returned id is `json_extract` of that record. Nothing
    /// here deserializes an admission.
    pub fn list_settled_leaves_pending_reclaim(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<String>, OrbitError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT json_extract(record_json,'$.leaf_run_id') FROM local_pull_admissions \
                 WHERE workspace_id=?1 AND leaf_run_id IS NOT NULL \
                 AND json_extract(record_json,'$.phase')='settled' \
                 AND json_extract(record_json,'$.leaf_run_id') IS NOT NULL \
                 AND json_extract(record_json,'$.build_output_reclaim_complete') IS NOT 1 \
                 ORDER BY rowid",
            )
            .map_err(sqlite_store_error)?;
        stmt.query_map([workspace_id], |row| row.get::<_, String>(0))
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    /// Visit the settled claims drain `run_id` admitted against
    /// `destination`, newest first, until `visit` returns `false`.
    ///
    /// `claim_id IS NOT NULL` is the cheap column test that lets SQLite skip
    /// the idle polls and refused requests, the rows that pile up unbounded,
    /// without parsing their JSON: a settlement requires a claim, so a settled
    /// row always carries one. Rows are decoded one at a time, so a visitor
    /// that stops early never decodes older history. SQL only narrows: the
    /// visitor re-checks each record.
    pub fn scan_settled_claims(
        &self,
        workspace_id: &str,
        destination: &PullDestination,
        run_id: &str,
        mut visit: impl FnMut(LocalPullAdmission) -> bool,
    ) -> Result<(), OrbitError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT record_json FROM local_pull_admissions \
                 WHERE workspace_id=?1 AND owner_machine=?2 AND owner_workspace=?3 \
                 AND execution_machine=?4 AND claim_id IS NOT NULL \
                 AND json_extract(record_json,'$.phase')='settled' \
                 AND json_extract(record_json,'$.request.run_context.run_id')=?5 \
                 AND json_extract(record_json,'$.destination.selector')=?6 \
                 ORDER BY rowid DESC",
            )
            .map_err(sqlite_store_error)?;
        let mut rows = stmt
            .query(params![
                workspace_id,
                destination.owner_machine_id,
                destination.owner_workspace_id,
                destination.execution_machine_id,
                run_id,
                destination.selector,
            ])
            .map_err(sqlite_store_error)?;
        while let Some(row) = rows.next().map_err(sqlite_store_error)? {
            let record = decode(row.get::<_, String>(0).map_err(sqlite_store_error)?)?;
            if !visit(record) {
                break;
            }
        }
        Ok(())
    }

    /// Every admission drain `run_id` made that holds or held a claim, any
    /// phase, in admission order. `claim_id IS NOT NULL` skips the idle polls
    /// and refused requests without parsing their JSON.
    pub fn list_claims_admitted_by(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Vec<LocalPullAdmission>, OrbitError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT record_json FROM local_pull_admissions \
                 WHERE workspace_id=?1 AND claim_id IS NOT NULL \
                 AND json_extract(record_json,'$.request.run_context.run_id')=?2 \
                 ORDER BY rowid",
            )
            .map_err(sqlite_store_error)?;
        let raw = stmt
            .query_map(params![workspace_id, run_id], |r| r.get::<_, String>(0))
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)?;
        let mut claims = Vec::with_capacity(raw.len());
        for record in raw.into_iter().map(decode) {
            let record = record?;
            // SQL only narrows; the record stays the authority.
            if record.request.run_context.run_id == run_id {
                claims.push(record);
            }
        }
        Ok(claims)
    }
}

fn decode(raw: String) -> Result<LocalPullAdmission, OrbitError> {
    serde_json::from_str(&raw).map_err(|error| OrbitError::Store(error.to_string()))
}
