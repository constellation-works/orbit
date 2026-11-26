//! Concrete repository for host review records.

mod ledger;
mod reconciliation;

use crate::store::{Snapshot, Tx};
use chrono::{DateTime, Utc};
use ledger::{decode, encode, ledgers_for_claim, ledgers_held_by, read_ledger};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::{ReviewCertificate, ReviewLanding, ReviewLedger, ReviewReconciliation};
use rusqlite::Connection;
use rusqlite::{OptionalExtension, params};
use std::marker::PhantomData;

/// Host review records scoped to a snapshot or write transaction.
pub struct ReviewRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}
impl<'a, 's> ReviewRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}
impl<'a, 't> ReviewRepository<'a, Tx<'t>> {
    pub(crate) fn for_tx(tx: &'a Tx<'t>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }
}

impl<Access> ReviewRepository<'_, Access> {
    pub fn ledger(
        &self,
        workspace_id: &str,
        lineage_key: &str,
    ) -> Result<Option<ReviewLedger>, OrbitError> {
        read_ledger(self.conn, workspace_id, lineage_key)
    }

    pub fn ledgers_held_by(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Vec<ReviewLedger>, OrbitError> {
        ledgers_held_by(self.conn, workspace_id, run_id)
    }

    pub fn ledgers_for_claim(
        &self,
        workspace_id: &str,
        claim_id: &str,
        task_id: &str,
    ) -> Result<Vec<ReviewLedger>, OrbitError> {
        if claim_id.is_empty() || task_id.is_empty() {
            return Ok(Vec::new());
        }
        {
            let mut ledgers = ledgers_for_claim(self.conn, workspace_id, claim_id)?;
            ledgers.retain(|ledger| ledger.task_ids.iter().any(|id| id == task_id));
            Ok(ledgers)
        }
    }

    pub fn certificate(
        &self,
        workspace_id: &str,
        attempt_id: &str,
    ) -> Result<Option<ReviewCertificate>, OrbitError> {
        let raw: Option<String> = self.conn
                .query_row(
                    "SELECT certificate_json FROM review_certificates WHERE attempt_id=?1 AND workspace_id=?2",
                    params![attempt_id, workspace_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sqlite_store_error)?;
        raw.as_deref().map(decode).transpose()
    }

    pub fn certificates_for_tree(
        &self,
        repository: &str,
        final_candidate_tree: &str,
        limit: usize,
    ) -> Result<Vec<ReviewCertificate>, OrbitError> {
        let mut stmt = self.conn
                .prepare(
                    "SELECT certificate_json FROM review_certificates WHERE repository=?1 AND candidate_tree=?2 AND passed=1 ORDER BY issued_at DESC LIMIT ?3",
                )
                .map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(
                params![repository, final_candidate_tree, limit.clamp(1, 100)],
                |row| row.get::<_, String>(0),
            )
            .map_err(sqlite_store_error)?;
        rows.map(|row| decode(&row.map_err(sqlite_store_error)?))
            .collect()
    }

    pub fn landings(&self, attempt_id: &str) -> Result<Vec<ReviewLanding>, OrbitError> {
        let mut stmt = self.conn
                .prepare(
                    "SELECT landing_json FROM review_landings WHERE attempt_id=?1 ORDER BY recorded_at ASC LIMIT 100",
                )
                .map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map([attempt_id], |row| row.get::<_, String>(0))
            .map_err(sqlite_store_error)?;
        rows.map(|row| decode(&row.map_err(sqlite_store_error)?))
            .collect()
    }

    /// Compare an existing certificate against the exact bytes it would store.
    pub fn certificate_matches(
        &self,
        certificate: &ReviewCertificate,
    ) -> Result<Option<bool>, OrbitError> {
        let encoded = encode(certificate)?;
        self.conn
            .query_row(
                "SELECT certificate_json FROM review_certificates WHERE attempt_id=?1",
                [&certificate.attempt_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map(|raw| raw.map(|raw| raw == encoded))
            .map_err(sqlite_store_error)
    }

    pub fn reconciliation(
        &self,
        workspace_id: &str,
        id: &str,
    ) -> Result<Option<ReviewReconciliation>, OrbitError> {
        reconciliation::read(self.conn, workspace_id, id)
    }

    pub fn reconciliation_for_request(
        &self,
        workspace_id: &str,
        task_id: &str,
        request_key: &str,
    ) -> Result<Option<ReviewReconciliation>, OrbitError> {
        reconciliation::for_request(self.conn, workspace_id, task_id, request_key)
    }

    pub fn reconciliations_for_task(
        &self,
        workspace_id: &str,
        task_id: &str,
    ) -> Result<Vec<ReviewReconciliation>, OrbitError> {
        let mut statement = self.conn
                .prepare(
                    "SELECT record_json FROM review_reconciliations WHERE workspace_id=?1 AND task_id=?2 ORDER BY updated_at DESC, reconciliation_id DESC",
                )
                .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map(params![workspace_id, task_id], |row| {
                row.get::<_, String>(0)
            })
            .map_err(sqlite_store_error)?;
        rows.map(|raw| raw.map_err(sqlite_store_error).and_then(|raw| decode(&raw)))
            .collect()
    }
}
impl ReviewRepository<'_, Tx<'_>> {
    /// Write a ledger against its previous revision, or insert its first revision.
    pub fn put_ledger(
        &self,
        workspace_id: &str,
        revision: Option<u32>,
        ledger: &mut ReviewLedger,
        now: DateTime<Utc>,
    ) -> Result<(), OrbitError> {
        ledger::write_ledger(self.conn, workspace_id, revision, ledger, now)
    }

    pub fn insert_certificate(
        &self,
        workspace_id: &str,
        certificate: &ReviewCertificate,
    ) -> Result<(), OrbitError> {
        self.conn
            .execute(
                "INSERT INTO review_certificates VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    certificate.attempt_id,
                    workspace_id,
                    certificate.repository,
                    certificate.final_candidate.commit,
                    certificate.final_candidate.tree,
                    i64::from(certificate.verdict.passed()),
                    encode(certificate)?,
                    certificate.issued_at.to_rfc3339()
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }

    /// Insert the landing for this attempt and commit, ignoring replay.
    pub fn insert_landing(&self, landing: &ReviewLanding) -> Result<(), OrbitError> {
        let record_id = format!("{}:{}", landing.attempt_id, landing.landed.commit);
        let encoded = encode(landing)?;
        self.conn
            .execute(
                "INSERT INTO review_landings VALUES (?1,?2,?3,?4,?5)
             ON CONFLICT(record_id) DO NOTHING",
                params![
                    record_id,
                    landing.attempt_id,
                    landing.landed.commit,
                    encoded,
                    landing.recorded_at.to_rfc3339()
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }

    pub fn insert_reconciliation(
        &self,
        workspace_id: &str,
        record: &ReviewReconciliation,
    ) -> Result<(), OrbitError> {
        reconciliation::insert(self.conn, workspace_id, record)
    }

    pub fn replace_reconciliation(
        &self,
        workspace_id: &str,
        revision: u32,
        record: &ReviewReconciliation,
    ) -> Result<(), OrbitError> {
        reconciliation::replace(self.conn, workspace_id, revision, record)
    }
}
