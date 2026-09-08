//! Before-PR review ledgers, certificates, and landing records in the host
//! SQLite database [ORB-11333].

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::{
    ReviewAttempt, ReviewAttemptState, ReviewCertificate, ReviewLanding, ReviewLedger,
    ReviewReservation, ReviewVerdict,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::Store;
use crate::contracts::{ReviewReserveRequest, ReviewSettlement, ReviewStoreBackend};
use crate::driver::sqlite::migration::FeatureMigration;

#[cfg(test)]
mod tests;

pub(crate) fn initialize(store: &Store) -> Result<(), OrbitError> {
    store.apply_feature_migrations(
        "review",
        &[FeatureMigration::new(1, "lineages_certificates_landings", |conn| {
            conn.execute_batch(
                "CREATE TABLE review_lineages (workspace_id TEXT NOT NULL, lineage_key TEXT NOT NULL, revision INTEGER NOT NULL, ledger_json TEXT NOT NULL, PRIMARY KEY(workspace_id, lineage_key));
                 CREATE TABLE review_certificates (attempt_id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL, repository TEXT NOT NULL, candidate_commit TEXT NOT NULL, candidate_tree TEXT NOT NULL, passed INTEGER NOT NULL, certificate_json TEXT NOT NULL, issued_at TEXT NOT NULL);
                 CREATE INDEX review_certificates_tree ON review_certificates(repository, candidate_tree, issued_at);
                 CREATE TABLE review_landings (record_id TEXT PRIMARY KEY, attempt_id TEXT NOT NULL, landed_commit TEXT NOT NULL, landing_json TEXT NOT NULL, recorded_at TEXT NOT NULL);
                 CREATE INDEX review_landings_attempt ON review_landings(attempt_id, recorded_at);",
            )
            .map_err(|error| OrbitError::Store(error.to_string()))
        })],
    )
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, OrbitError> {
    serde_json::to_string(value).map_err(|error| OrbitError::Store(error.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, OrbitError> {
    serde_json::from_str(raw)
        .map_err(|error| OrbitError::Store(format!("invalid persisted review record: {error}")))
}

fn read_ledger(
    conn: &Connection,
    workspace_id: &str,
    lineage_key: &str,
) -> Result<Option<ReviewLedger>, OrbitError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT ledger_json FROM review_lineages WHERE workspace_id=?1 AND lineage_key=?2",
            params![workspace_id, lineage_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    raw.as_deref().map(decode).transpose()
}

/// Write the ledger, fencing on the revision the caller read.
fn write_ledger(
    conn: &Connection,
    workspace_id: &str,
    previous_revision: Option<u32>,
    ledger: &mut ReviewLedger,
    now: DateTime<Utc>,
) -> Result<(), OrbitError> {
    ledger.updated_at = now;
    match previous_revision {
        None => {
            ledger.revision = 1;
            conn.execute(
                "INSERT INTO review_lineages VALUES (?1,?2,?3,?4)",
                params![
                    workspace_id,
                    ledger.lineage_key,
                    ledger.revision,
                    encode(ledger)?
                ],
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        }
        Some(previous) => {
            ledger.revision = previous.saturating_add(1);
            let changed = conn
                .execute(
                    "UPDATE review_lineages SET revision=?1, ledger_json=?2 WHERE workspace_id=?3 AND lineage_key=?4 AND revision=?5",
                    params![
                        ledger.revision,
                        encode(ledger)?,
                        workspace_id,
                        ledger.lineage_key,
                        previous
                    ],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if changed == 0 {
                return Err(OrbitError::Store(
                    "review ledger changed concurrently; reread and retry".into(),
                ));
            }
        }
    }
    Ok(())
}

fn attempt_id(lineage_key: &str, index: u32) -> String {
    let digest = format!("{:x}", Sha256::digest(lineage_key.as_bytes()));
    format!("rvw-{}-{index}", &digest[..12])
}

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
            // different candidate it is partial work, settled as incomplete
            // with the wall time already spent.
            if let Some(open) = ledger.open_attempt().cloned() {
                if open.candidate == *request.candidate
                    && open.task_meaning_digest == request.task_meaning_digest
                {
                    return Ok((ReviewReservation::Resumed { attempt: open }, ledger));
                }
                settle_attempt(
                    &mut ledger,
                    &open.attempt_id,
                    ReviewVerdict::Incomplete,
                    0,
                    open.elapsed_at(request.now),
                );
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
                settlement.elapsed_seconds,
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

/// Mark an attempt settled. Returns false when the attempt is unknown.
fn settle_attempt(
    ledger: &mut ReviewLedger,
    attempt_id: &str,
    verdict: ReviewVerdict,
    repair_cycles: u32,
    elapsed_seconds: u64,
) -> bool {
    let Some(attempt) = ledger
        .attempts
        .iter_mut()
        .find(|attempt| attempt.attempt_id == attempt_id)
    else {
        return false;
    };
    attempt.state = ReviewAttemptState::Settled { verdict };
    attempt.repair_cycles = repair_cycles;
    attempt.elapsed_seconds = Some(elapsed_seconds);
    ledger.consumed_seconds = ledger.consumed_seconds.saturating_add(elapsed_seconds);
    true
}

fn reserve_new(
    conn: &Connection,
    workspace_id: &str,
    ledger: ReviewLedger,
    request: &ReviewReserveRequest<'_>,
) -> Result<(ReviewReservation, ReviewLedger), OrbitError> {
    let revision = ledger.revision;
    reserve_new_in(conn, workspace_id, Some(revision), ledger, request)
}

fn reserve_new_in(
    conn: &Connection,
    workspace_id: &str,
    previous_revision: Option<u32>,
    mut ledger: ReviewLedger,
    request: &ReviewReserveRequest<'_>,
) -> Result<(ReviewReservation, ReviewLedger), OrbitError> {
    let consumed = ledger.consumed();
    if consumed.reviewer_starts >= ledger.budget.reviewer_starts {
        return Ok((
            ReviewReservation::Exhausted {
                reason: "review_starts_exhausted",
                consumed,
            },
            ledger,
        ));
    }
    if consumed.seconds >= u64::from(ledger.budget.minutes).saturating_mul(60) {
        return Ok((
            ReviewReservation::Exhausted {
                reason: "review_minutes_exhausted",
                consumed,
            },
            ledger,
        ));
    }

    let index = consumed.reviewer_starts.saturating_add(1);
    let attempt = ReviewAttempt {
        attempt_id: attempt_id(request.lineage_key, index),
        index,
        run_id: request.run_id.to_string(),
        task_meaning_digest: request.task_meaning_digest.to_string(),
        candidate: request.candidate.clone(),
        started_at: request.now,
        state: ReviewAttemptState::Open,
        repair_cycles: 0,
        elapsed_seconds: None,
    };
    ledger.attempts.push(attempt.clone());
    write_ledger(
        conn,
        workspace_id,
        previous_revision,
        &mut ledger,
        request.now,
    )?;
    Ok((ReviewReservation::Reserved { attempt }, ledger))
}
