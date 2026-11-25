//! Concrete repository over the host-only recovery authority database
//! (`authority.db`).

use std::path::Path;

use orbit_common::OrbitError;
use rusqlite::{Connection, OptionalExtension, params};

use crate::driver::sqlite::authority::{AuthorityDb, FILE_NAME, authority_error};

/// One reserved rebase recovery attempt scope persisted in `authority.db`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryAttemptRecord {
    pub workspace_path: String,
    pub head_sha_before: String,
    pub target_base_sha: String,
}

/// One rebase recovery certificate row, either attempt-bound or pre-attempt legacy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryCertificateRecord {
    pub run_id: String,
    pub step_id: String,
    pub attempt: Option<i64>,
    pub workspace_path: String,
    pub head_sha: String,
    pub base_sha: String,
    pub payload_digest: String,
}

/// Summary row of a persisted attempt certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptCertificateRow {
    pub run_id: String,
    pub step_id: String,
    pub attempt: i64,
    pub payload_digest: String,
    pub issued_at: String,
}

/// Concrete repository for read-write access to `authority.db`.
#[derive(Debug)]
pub struct RecoveryAuthorityStore {
    db: AuthorityDb,
}

/// Active `BEGIN IMMEDIATE` transaction over `authority.db`.
pub struct RecoveryAuthorityTx<'a> {
    connection: &'a Connection,
}

/// Concrete read-only repository for sandboxed worker lookups in `authority.db`.
#[derive(Debug)]
pub struct RecoveryAuthorityReadOnly {
    db: AuthorityDb,
}

impl RecoveryAuthorityStore {
    /// File name of the isolated recovery authority database.
    pub const FILE_NAME: &'static str = FILE_NAME;

    /// Open (creating and migrating on first use) the authority database at `path`.
    pub fn open(path: &Path) -> Result<Self, OrbitError> {
        Ok(Self {
            db: AuthorityDb::open(path)?,
        })
    }

    #[cfg(test)]
    pub(crate) fn connection(&self) -> &Connection {
        self.db.connection()
    }

    /// Run `body` inside one `BEGIN IMMEDIATE` transaction.
    pub fn write<T>(
        &self,
        action: &str,
        body: impl FnOnce(&RecoveryAuthorityTx<'_>) -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        self.db.immediate(action, |connection| {
            body(&RecoveryAuthorityTx { connection })
        })
    }

    /// Highest certified attempt number for `run_id` / `step_id`, if any.
    pub fn latest_certified_attempt(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Option<i64>, OrbitError> {
        self.db
            .connection()
            .query_row(
                "SELECT MAX(attempt) FROM rebase_recovery_attempt_certificate
                 WHERE run_id = ?1 AND step_id = ?2",
                params![run_id, step_id],
                |row| row.get(0),
            )
            .map_err(|error| authority_error("read rebase recovery certificates", error))
    }

    /// Read the attempt-bound certificate for `run_id` / `step_id` / `attempt`.
    pub fn certificate(
        &self,
        run_id: &str,
        step_id: &str,
        attempt: i64,
    ) -> Result<Option<RecoveryCertificateRecord>, OrbitError> {
        read_certificate(self.db.connection(), run_id, step_id, attempt)
    }

    /// Read the pre-attempt legacy certificate for `run_id` / `step_id`.
    pub fn legacy_certificate(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Option<RecoveryCertificateRecord>, OrbitError> {
        self.db
            .connection()
            .query_row(
                "SELECT workspace_path, head_sha, base_sha, payload_digest
                 FROM rebase_recovery_certificate
                 WHERE run_id = ?1 AND step_id = ?2",
                params![run_id, step_id],
                |row| {
                    Ok(RecoveryCertificateRecord {
                        run_id: run_id.to_string(),
                        step_id: step_id.to_string(),
                        attempt: None,
                        workspace_path: row.get(0)?,
                        head_sha: row.get(1)?,
                        base_sha: row.get(2)?,
                        payload_digest: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|error| authority_error("read legacy rebase recovery certificate", error))
    }

    /// List every attempt reserved for `run_id` / `step_id`, oldest first.
    pub fn list_attempts(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Vec<RecoveryAttemptRecord>, OrbitError> {
        let mut statement = self
            .db
            .connection()
            .prepare(
                "SELECT workspace_path, head_sha_before, target_base_sha
                 FROM rebase_recovery_attempt
                 WHERE run_id = ?1 AND step_id = ?2
                 ORDER BY attempt",
            )
            .map_err(|error| authority_error("read rebase recovery attempts", error))?;
        let rows = statement
            .query_map(params![run_id, step_id], |row| {
                Ok(RecoveryAttemptRecord {
                    workspace_path: row.get(0)?,
                    head_sha_before: row.get(1)?,
                    target_base_sha: row.get(2)?,
                })
            })
            .map_err(|error| authority_error("read rebase recovery attempts", error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| authority_error("read rebase recovery attempts", error))
    }

    /// List every persisted attempt certificate row ordered by `(run_id, step_id, attempt)`.
    pub fn list_attempt_certificates(&self) -> Result<Vec<AttemptCertificateRow>, OrbitError> {
        let mut statement = self
            .db
            .connection()
            .prepare(
                "SELECT run_id, step_id, attempt, payload_digest, issued_at
                 FROM rebase_recovery_attempt_certificate
                 ORDER BY run_id, step_id, attempt",
            )
            .map_err(|error| authority_error("read rebase recovery certificates", error))?;
        let rows = statement
            .query_map([], |row| {
                Ok(AttemptCertificateRow {
                    run_id: row.get(0)?,
                    step_id: row.get(1)?,
                    attempt: row.get(2)?,
                    payload_digest: row.get(3)?,
                    issued_at: row.get(4)?,
                })
            })
            .map_err(|error| authority_error("read rebase recovery certificates", error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| authority_error("read rebase recovery certificates", error))
    }

    /// Insert a worker process binding if absent and return the persisted JSON.
    pub fn insert_or_get_worker_process_binding(
        &self,
        pid: u32,
        identity: &str,
        binding_json: &str,
    ) -> Result<String, OrbitError> {
        self.db
            .connection()
            .execute(
                "INSERT INTO worker_process_binding(pid, identity, binding_json)
                 VALUES (?1,?2,?3) ON CONFLICT(pid,identity) DO NOTHING",
                params![pid, identity, binding_json],
            )
            .map_err(|error| authority_error("bind worker process", error))?;
        self.db
            .connection()
            .query_row(
                "SELECT binding_json FROM worker_process_binding WHERE pid=?1 AND identity=?2",
                params![pid, identity],
                |row| row.get(0),
            )
            .map_err(|error| authority_error("read worker process binding", error))
    }

    /// Insert a worker namespace binding if absent and return the persisted JSON.
    pub fn insert_or_get_worker_namespace_binding(
        &self,
        namespace_key: &str,
        binding_json: &str,
    ) -> Result<String, OrbitError> {
        self.db
            .connection()
            .execute(
                "INSERT INTO worker_namespace_binding(namespace_key,binding_json)
                 VALUES (?1,?2) ON CONFLICT(namespace_key) DO NOTHING",
                params![namespace_key, binding_json],
            )
            .map_err(|error| authority_error("record worker namespace", error))?;
        self.db
            .connection()
            .query_row(
                "SELECT binding_json FROM worker_namespace_binding WHERE namespace_key=?1",
                params![namespace_key],
                |row| row.get(0),
            )
            .map_err(|error| authority_error("read worker namespace", error))
    }
}

impl RecoveryAuthorityTx<'_> {
    /// Highest reserved attempt number for `run_id` / `step_id`, if any.
    pub fn latest_reservation(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Option<i64>, OrbitError> {
        self.connection
            .query_row(
                "SELECT MAX(attempt) FROM rebase_recovery_attempt
                 WHERE run_id = ?1 AND step_id = ?2",
                params![run_id, step_id],
                |row| row.get(0),
            )
            .map_err(|error| authority_error("read rebase recovery attempts", error))
    }

    /// Read one reserved attempt scope for `run_id` / `step_id` / `attempt`.
    pub fn reservation(
        &self,
        run_id: &str,
        step_id: &str,
        attempt: i64,
    ) -> Result<Option<RecoveryAttemptRecord>, OrbitError> {
        self.connection
            .query_row(
                "SELECT workspace_path, head_sha_before, target_base_sha
                 FROM rebase_recovery_attempt
                 WHERE run_id = ?1 AND step_id = ?2 AND attempt = ?3",
                params![run_id, step_id, attempt],
                |row| {
                    Ok(RecoveryAttemptRecord {
                        workspace_path: row.get(0)?,
                        head_sha_before: row.get(1)?,
                        target_base_sha: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(|error| authority_error("read rebase recovery attempt", error))
    }

    /// Insert a new reserved attempt row.
    pub fn insert_reservation(
        &self,
        run_id: &str,
        step_id: &str,
        attempt: i64,
        record: &RecoveryAttemptRecord,
        reserved_at: &str,
    ) -> Result<(), OrbitError> {
        self.connection
            .execute(
                "INSERT INTO rebase_recovery_attempt
                     (run_id, step_id, attempt, workspace_path, head_sha_before,
                      target_base_sha, reserved_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    run_id,
                    step_id,
                    attempt,
                    record.workspace_path,
                    record.head_sha_before,
                    record.target_base_sha,
                    reserved_at,
                ],
            )
            .map_err(|error| authority_error("reserve rebase recovery attempt", error))?;
        Ok(())
    }

    /// Read an existing attempt certificate row inside the transaction.
    pub fn certificate(
        &self,
        run_id: &str,
        step_id: &str,
        attempt: i64,
    ) -> Result<Option<RecoveryCertificateRecord>, OrbitError> {
        read_certificate(self.connection, run_id, step_id, attempt)
    }

    /// Insert a new attempt certificate row.
    pub fn insert_certificate(
        &self,
        record: &RecoveryCertificateRecord,
        attempt: i64,
        issued_at: &str,
    ) -> Result<(), OrbitError> {
        self.connection
            .execute(
                "INSERT INTO rebase_recovery_attempt_certificate
                     (run_id, step_id, attempt, workspace_path, head_sha, base_sha,
                      payload_digest, issued_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    record.run_id,
                    record.step_id,
                    attempt,
                    record.workspace_path,
                    record.head_sha,
                    record.base_sha,
                    record.payload_digest,
                    issued_at,
                ],
            )
            .map_err(|error| authority_error("record rebase recovery certificate", error))?;
        Ok(())
    }
}

impl RecoveryAuthorityReadOnly {
    /// Open an existing `authority.db` read-only without running migrations.
    pub fn open(path: &Path) -> Result<Self, OrbitError> {
        Ok(Self {
            db: AuthorityDb::open_read_only(path)?,
        })
    }

    /// Whether the optional pre-ledger `worker_namespace_binding` table exists.
    pub fn has_worker_namespace_table(&self) -> Result<bool, OrbitError> {
        self.db
            .connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='worker_namespace_binding')",
                [],
                |row| row.get(0),
            )
            .map_err(|error| authority_error("inspect namespace authority", error))
    }

    /// Look up a worker namespace binding JSON by `namespace_key`.
    pub fn worker_namespace_binding(
        &self,
        namespace_key: &str,
    ) -> Result<Option<String>, OrbitError> {
        self.db
            .connection()
            .query_row(
                "SELECT binding_json FROM worker_namespace_binding WHERE namespace_key=?1",
                params![namespace_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| authority_error("resolve namespace authority", error))
    }

    /// Whether the optional pre-ledger `worker_process_binding` table exists.
    pub fn has_worker_process_table(&self) -> Result<bool, OrbitError> {
        self.db
            .connection()
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='worker_process_binding')",
                [],
                |row| row.get(0),
            )
            .map_err(|error| authority_error("inspect worker authority", error))
    }

    /// Look up a worker process binding JSON by `(pid, identity)`.
    pub fn worker_process_binding(
        &self,
        pid: u32,
        identity: &str,
    ) -> Result<Option<String>, OrbitError> {
        self.db
            .connection()
            .query_row(
                "SELECT binding_json FROM worker_process_binding WHERE pid=?1 AND identity=?2",
                params![pid, identity],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| authority_error("resolve worker process", error))
    }
}

fn read_certificate(
    connection: &Connection,
    run_id: &str,
    step_id: &str,
    attempt: i64,
) -> Result<Option<RecoveryCertificateRecord>, OrbitError> {
    connection
        .query_row(
            "SELECT workspace_path, head_sha, base_sha, payload_digest
             FROM rebase_recovery_attempt_certificate
             WHERE run_id = ?1 AND step_id = ?2 AND attempt = ?3",
            params![run_id, step_id, attempt],
            |row| {
                Ok(RecoveryCertificateRecord {
                    run_id: run_id.to_string(),
                    step_id: step_id.to_string(),
                    attempt: Some(attempt),
                    workspace_path: row.get(0)?,
                    head_sha: row.get(1)?,
                    base_sha: row.get(2)?,
                    payload_digest: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(|error| authority_error("read rebase recovery certificate", error))
}

/// Test-only fixture helper: lay out `authority.db` in the pre-attempt legacy
/// shape (only `rebase_recovery_certificate`, no ledger or attempt tables) and
/// insert one legacy certificate row.
#[cfg(any(test, feature = "test-support"))]
pub fn seed_legacy_authority_certificate(
    path: &Path,
    record: &RecoveryCertificateRecord,
    issued_at: &str,
) -> Result<(), OrbitError> {
    let db = AuthorityDb::open(path)?;
    db.connection()
        .execute_batch(
            "DROP TABLE rebase_recovery_attempt;
             DROP TABLE rebase_recovery_attempt_certificate;
             DROP TABLE worker_process_binding;
             DROP TABLE worker_namespace_binding;
             DROP TABLE schema_meta;",
        )
        .map_err(|error| authority_error("strip attempt tables", error))?;
    db.connection()
        .execute(
            "INSERT INTO rebase_recovery_certificate
                 (run_id, step_id, workspace_path, head_sha, base_sha, payload_digest, issued_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.run_id,
                record.step_id,
                record.workspace_path,
                record.head_sha,
                record.base_sha,
                record.payload_digest,
                issued_at,
            ],
        )
        .map_err(|error| authority_error("write legacy certificate", error))?;
    Ok(())
}
