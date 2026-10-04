//! ReviewStoreBackend: ledger reserve/settle, certificate record/query, and landings.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::{
    ReviewAttemptState, ReviewCertificate, ReviewLanding, ReviewLedger, ReviewReservation,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::attempts::{
    record_invocation, release_attempt, reserve_new, reserve_new_in, settle_attempt,
};
use super::ledger::{decode, encode, ledgers_held_by, read_ledger, write_ledger};
use crate::Store;
use crate::contracts::{
    ReviewInvocationRecord, ReviewRelease, ReviewReserveRequest, ReviewSettlement,
    ReviewStoreBackend,
};

impl ReviewStoreBackend for Store {
    fn review_ledger(
        &self,
        workspace_id: &str,
        lineage_key: &str,
    ) -> Result<Option<ReviewLedger>, OrbitError> {
        self.with_read_connection(|conn| read_ledger(conn, workspace_id, lineage_key))
    }

    fn review_reserve(
        &self,
        workspace_id: &str,
        request: &ReviewReserveRequest<'_>,
    ) -> Result<(ReviewReservation, ReviewLedger), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let existing = read_ledger(conn, workspace_id, request.lineage_key)?;
            let previous_revision = existing.as_ref().map(|ledger| ledger.revision);
            let mut ledger = existing.unwrap_or_else(|| {
                ReviewLedger::new(
                    request.lineage_key,
                    request.task_ids.to_vec(),
                    request.budget,
                    request.now,
                )
            });

            // An interrupted attempt on the same candidate resumes; on a
            // different candidate it is partial work, released as incomplete
            // with the reviewer runtime already spent.
            if let Some(open) = ledger.open_attempt().cloned() {
                if open.candidate == *request.candidate
                    && open.task_meaning_digest == request.task_meaning_digest
                {
                    return Ok((ReviewReservation::Resumed { attempt: open }, ledger));
                }
                release_attempt(&mut ledger, &open.attempt_id, request.now, request.now);
                write_ledger(
                    conn,
                    workspace_id,
                    previous_revision,
                    &mut ledger,
                    request.now,
                )?;
                return reserve_new(conn, workspace_id, ledger, request);
            }

            let reserved = reserve_new_in(conn, workspace_id, previous_revision, ledger, request)?;
            Ok(reserved)
        })
    }

    fn review_settle(
        &self,
        workspace_id: &str,
        settlement: &ReviewSettlement<'_>,
    ) -> Result<ReviewLedger, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let mut ledger =
                read_ledger(conn, workspace_id, settlement.lineage_key)?.ok_or_else(|| {
                    OrbitError::Store(format!(
                        "review lineage '{}' has no ledger to settle",
                        settlement.lineage_key
                    ))
                })?;
            let previous_revision = ledger.revision;
            let already_settled = ledger.attempts.iter().any(|attempt| {
                attempt.attempt_id == settlement.attempt_id
                    && attempt.released_at.is_none()
                    && matches!(attempt.state, ReviewAttemptState::Settled { .. })
            });
            if already_settled {
                return Ok(ledger);
            }
            if !settle_attempt(
                &mut ledger,
                settlement.attempt_id,
                settlement.verdict,
                settlement.repair_cycles,
                settlement.now,
            ) {
                return Err(OrbitError::Store(format!(
                    "review attempt '{}' is not part of lineage '{}'",
                    settlement.attempt_id, settlement.lineage_key
                )));
            }
            write_ledger(
                conn,
                workspace_id,
                Some(previous_revision),
                &mut ledger,
                settlement.now,
            )?;
            Ok(ledger)
        })
    }

    fn review_release(
        &self,
        workspace_id: &str,
        release: &ReviewRelease<'_>,
    ) -> Result<ReviewLedger, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let mut ledger =
                read_ledger(conn, workspace_id, release.lineage_key)?.ok_or_else(|| {
                    OrbitError::Store(format!(
                        "review lineage '{}' has no ledger to release",
                        release.lineage_key
                    ))
                })?;
            let previous_revision = ledger.revision;
            if !ledger
                .attempts
                .iter()
                .any(|attempt| attempt.attempt_id == release.attempt_id)
            {
                return Err(OrbitError::Store(format!(
                    "review attempt '{}' is not part of lineage '{}'",
                    release.attempt_id, release.lineage_key
                )));
            }
            if release_attempt(&mut ledger, release.attempt_id, release.bound, release.now) {
                write_ledger(
                    conn,
                    workspace_id,
                    Some(previous_revision),
                    &mut ledger,
                    release.now,
                )?;
            }
            Ok(ledger)
        })
    }

    fn review_release_run(
        &self,
        workspace_id: &str,
        run_id: &str,
        finished_at: DateTime<Utc>,
    ) -> Result<Vec<ReviewLedger>, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let mut released = Vec::new();
            for mut ledger in ledgers_held_by(conn, workspace_id, run_id)? {
                let previous_revision = ledger.revision;
                let held = ledger
                    .attempts
                    .iter()
                    .filter(|attempt| attempt.holder_run_id() == Some(run_id))
                    .map(|attempt| attempt.attempt_id.clone())
                    .collect::<Vec<_>>();
                let mut changed = false;
                for attempt_id in &held {
                    changed |= release_attempt(&mut ledger, attempt_id, finished_at, finished_at);
                }
                if changed {
                    write_ledger(
                        conn,
                        workspace_id,
                        Some(previous_revision),
                        &mut ledger,
                        finished_at,
                    )?;
                    released.push(ledger);
                }
            }
            Ok(released)
        })
    }

    fn review_record_invocation(
        &self,
        workspace_id: &str,
        record: &ReviewInvocationRecord<'_>,
    ) -> Result<ReviewLedger, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let mut ledger =
                read_ledger(conn, workspace_id, record.lineage_key)?.ok_or_else(|| {
                    OrbitError::Store(format!(
                        "review lineage '{}' has no ledger to record a reviewer invocation",
                        record.lineage_key
                    ))
                })?;
            let previous_revision = ledger.revision;
            if record_invocation(
                &mut ledger,
                record.attempt_id,
                record.run_id,
                record.event,
                record.now,
            ) {
                write_ledger(
                    conn,
                    workspace_id,
                    Some(previous_revision),
                    &mut ledger,
                    record.now,
                )?;
            }
            Ok(ledger)
        })
    }

    fn review_certificate_record(
        &self,
        workspace_id: &str,
        certificate: &ReviewCertificate,
    ) -> Result<(), OrbitError> {
        let encoded = encode(certificate)?;
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let existing: Option<String> = conn
                .query_row(
                    "SELECT certificate_json FROM review_certificates WHERE attempt_id=?1",
                    [&certificate.attempt_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if let Some(raw) = existing {
                if raw == encoded {
                    return Ok(());
                }
                return Err(OrbitError::InvalidInput(format!(
                    "review certificate '{}' already recorded with different content",
                    certificate.attempt_id
                )));
            }
            conn.execute(
                "INSERT INTO review_certificates VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    certificate.attempt_id,
                    workspace_id,
                    certificate.repository,
                    certificate.final_candidate.commit,
                    certificate.final_candidate.tree,
                    i64::from(certificate.verdict.passed()),
                    encoded,
                    certificate.issued_at.to_rfc3339()
                ],
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(())
        })
    }

    fn review_certificate(
        &self,
        workspace_id: &str,
        attempt_id: &str,
    ) -> Result<Option<ReviewCertificate>, OrbitError> {
        self.with_read_connection(|conn| {
            let raw: Option<String> = conn
                .query_row(
                    "SELECT certificate_json FROM review_certificates WHERE attempt_id=?1 AND workspace_id=?2",
                    params![attempt_id, workspace_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            raw.as_deref().map(decode).transpose()
        })
    }

    fn review_certificates_for_tree(
        &self,
        repository: &str,
        final_candidate_tree: &str,
        limit: usize,
    ) -> Result<Vec<ReviewCertificate>, OrbitError> {
        self.with_read_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT certificate_json FROM review_certificates WHERE repository=?1 AND candidate_tree=?2 AND passed=1 ORDER BY issued_at DESC LIMIT ?3",
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            let rows = stmt
                .query_map(
                    params![repository, final_candidate_tree, limit.clamp(1, 100)],
                    |row| row.get::<_, String>(0),
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            rows.map(|row| decode(&row.map_err(|error| OrbitError::Store(error.to_string()))?))
                .collect()
        })
    }

    fn review_landing_record(&self, landing: &ReviewLanding) -> Result<(), OrbitError> {
        let record_id = format!("{}:{}", landing.attempt_id, landing.landed.commit);
        let encoded = encode(landing)?;
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let existing: Option<String> = conn
                .query_row(
                    "SELECT landing_json FROM review_landings WHERE record_id=?1",
                    [&record_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if existing.is_some() {
                // A landing is a fact about one commit; replay changes nothing.
                return Ok(());
            }
            conn.execute(
                "INSERT INTO review_landings VALUES (?1,?2,?3,?4,?5)",
                params![
                    record_id,
                    landing.attempt_id,
                    landing.landed.commit,
                    encoded,
                    landing.recorded_at.to_rfc3339()
                ],
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(())
        })
    }

    fn review_landings(&self, attempt_id: &str) -> Result<Vec<ReviewLanding>, OrbitError> {
        self.with_read_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT landing_json FROM review_landings WHERE attempt_id=?1 ORDER BY recorded_at ASC LIMIT 100",
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            let rows = stmt
                .query_map([attempt_id], |row| row.get::<_, String>(0))
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            rows.map(|row| decode(&row.map_err(|error| OrbitError::Store(error.to_string()))?))
                .collect()
        })
    }
}
