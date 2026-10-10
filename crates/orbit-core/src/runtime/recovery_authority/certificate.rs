//! Recovery attempt reservations and immutable certificates.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::RebaseRecoveryAttemptScope;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use super::error::authority_error;
use super::root::{
    AUTHORITY_DB, authority_root, persist_wal_sidecars, refuse_symlinked_authority_files,
    restrict_permissions,
};

/// The checkpoint field carrying the host-assigned recovery attempt.
const RECOVERY_ATTEMPT_FIELD: &str = "recovery_attempt";

/// The durable record a host writes when it completes a rebase recovery, and
/// the only evidence a later resume will accept.
#[derive(Debug)]
pub(crate) struct RecoveryAuthority {
    // Certificate and worker-binding persistence share this authority connection.
    pub(super) connection: Connection,
}

/// The identity a certificate binds. Every field is re-derived from the
/// candidate checkpoint at verification time, so a payload that was edited,
/// moved to another run, step or attempt, or pointed at another workspace no
/// longer matches the record the host wrote.
#[derive(Debug, PartialEq, Eq)]
struct CertificateBinding {
    run_id: String,
    step_id: String,
    /// The host-assigned recovery attempt; `None` only for a payload written
    /// before attempts existed, which the legacy table alone can vouch for.
    attempt: Option<i64>,
    workspace_path: String,
    head_sha: String,
    base_sha: String,
    payload_digest: String,
}

impl RecoveryAuthority {
    /// Open (creating on first use) the authority for `global_root`.
    pub(crate) fn open(global_root: &Path) -> Result<Self, OrbitError> {
        let root = authority_root(global_root)?;
        refuse_symlinked_authority_files(&root)?;

        let connection = Connection::open(root.join(AUTHORITY_DB))
            .map_err(|error| authority_error("open recovery authority database", error))?;

        // WAL keeps the sidecars inside the protected root rather than beside
        // the shared store, and matches how every other Orbit database runs.
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| authority_error("enable WAL on recovery authority", error))?;
        persist_wal_sidecars(&connection)?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(|error| authority_error("harden recovery authority durability", error))?;
        // `rebase_recovery_certificate` is the pre-attempt shape, one row per
        // run and step. It stays exactly as older binaries create and read it:
        // this code never writes it, and only consults it for payloads that
        // carry no attempt.
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS rebase_recovery_certificate (
                     run_id         TEXT NOT NULL,
                     step_id        TEXT NOT NULL,
                     workspace_path TEXT NOT NULL,
                     head_sha       TEXT NOT NULL,
                     base_sha       TEXT NOT NULL,
                     payload_digest TEXT NOT NULL,
                     issued_at      TEXT NOT NULL,
                     PRIMARY KEY (run_id, step_id)
                 ) WITHOUT ROWID;
                 CREATE TABLE IF NOT EXISTS rebase_recovery_attempt (
                     run_id          TEXT NOT NULL,
                     step_id         TEXT NOT NULL,
                     attempt         INTEGER NOT NULL,
                     workspace_path  TEXT NOT NULL,
                     head_sha_before TEXT NOT NULL,
                     target_base_sha TEXT NOT NULL,
                     reserved_at     TEXT NOT NULL,
                     PRIMARY KEY (run_id, step_id, attempt)
                 ) WITHOUT ROWID;
                 CREATE TABLE IF NOT EXISTS rebase_recovery_attempt_certificate (
                     run_id         TEXT NOT NULL,
                     step_id        TEXT NOT NULL,
                     attempt        INTEGER NOT NULL,
                     workspace_path TEXT NOT NULL,
                     head_sha       TEXT NOT NULL,
                     base_sha       TEXT NOT NULL,
                     payload_digest TEXT NOT NULL,
                     issued_at      TEXT NOT NULL,
                     PRIMARY KEY (run_id, step_id, attempt)
                 ) WITHOUT ROWID;",
            )
            .map_err(|error| authority_error("create recovery authority schema", error))?;
        restrict_permissions(&root)?;

        Ok(Self { connection })
    }

    /// Reserve the next recovery attempt of `step_id` in `run_id`.
    ///
    /// The host calls this once per admitted conflict recovery, before the
    /// provider runs, and carries the returned number in memory to the
    /// checkpoint it later issues. The number is assigned here, never chosen
    /// by the caller, and each reservation supersedes every earlier one for
    /// the same run and step.
    pub(crate) fn begin_attempt(
        &self,
        run_id: &str,
        step_id: &str,
        scope: &RebaseRecoveryAttemptScope,
    ) -> Result<u64, OrbitError> {
        if [
            run_id,
            step_id,
            &scope.workspace_path,
            &scope.head_sha_before,
            &scope.target_base_sha,
        ]
        .iter()
        .any(|value| value.is_empty())
        {
            return Err(OrbitError::InvalidInput(
                "rebase recovery attempt needs a run, step, workspace, original HEAD and pinned base"
                    .to_string(),
            ));
        }
        self.immediate("reserve rebase recovery attempt", |connection| {
            let attempt = latest_reservation(connection, run_id, step_id)?.unwrap_or(0) + 1;
            connection
                .execute(
                    "INSERT INTO rebase_recovery_attempt
                         (run_id, step_id, attempt, workspace_path, head_sha_before,
                          target_base_sha, reserved_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        run_id,
                        step_id,
                        attempt,
                        scope.workspace_path,
                        scope.head_sha_before,
                        scope.target_base_sha,
                        Utc::now().to_rfc3339(),
                    ],
                )
                .map_err(|error| authority_error("reserve rebase recovery attempt", error))?;
            u64::try_from(attempt).map_err(|_| {
                OrbitError::Execution("rebase recovery attempt is out of range".to_string())
            })
        })
    }

    /// Record the host's completion of one reserved attempt of `step_id` in
    /// `run_id`; the attempt is the payload's `recovery_attempt`.
    ///
    /// Certificates are immutable: re-issuing the identical payload succeeds so
    /// a retried write is harmless, while different evidence for an attempt
    /// already certified is refused rather than replacing the accepted record.
    /// An attempt the host never reserved, one a later reservation superseded,
    /// and evidence for another workspace, original HEAD or pinned base than
    /// the reservation named are refused before anything is written.
    pub(crate) fn issue(
        &self,
        run_id: &str,
        step_id: &str,
        checkpoint: &Value,
    ) -> Result<(), OrbitError> {
        let binding = CertificateBinding::derive(run_id, step_id, checkpoint)?;
        let Some(attempt) = binding.attempt else {
            return Err(OrbitError::Execution(format!(
                "rebase recovery evidence for run {run_id} step `{step_id}` names no host-reserved \
                 attempt"
            )));
        };

        self.immediate("record rebase recovery certificate", |connection| {
            let Some(scope) = reservation(connection, run_id, step_id, attempt)? else {
                return Err(OrbitError::PolicyDenied(format!(
                    "rebase recovery attempt {attempt} for run {run_id} step `{step_id}` was not \
                     reserved by this host"
                )));
            };
            if latest_reservation(connection, run_id, step_id)? != Some(attempt) {
                return Err(OrbitError::PolicyDenied(format!(
                    "rebase recovery attempt {attempt} for run {run_id} step `{step_id}` was \
                     superseded by a later attempt"
                )));
            }
            if scope.workspace_path != binding.workspace_path
                || bound_field(checkpoint, "head_sha_before")? != scope.head_sha_before
                || bound_field(checkpoint, "target_base_sha")? != scope.target_base_sha
            {
                return Err(OrbitError::PolicyDenied(format!(
                    "rebase recovery evidence for run {run_id} step `{step_id}` attempt {attempt} \
                     does not describe the reserved workspace, original HEAD and pinned base"
                )));
            }

            if let Some(recorded) = certificate(connection, run_id, step_id, attempt)? {
                if recorded == binding {
                    return Ok(());
                }
                return Err(OrbitError::Execution(format!(
                    "rebase recovery for run {run_id} step `{step_id}` attempt {attempt} is \
                     already certified with different evidence; the accepted certificate is \
                     immutable"
                )));
            }

            connection
                .execute(
                    "INSERT INTO rebase_recovery_attempt_certificate
                         (run_id, step_id, attempt, workspace_path, head_sha, base_sha,
                          payload_digest, issued_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        binding.run_id,
                        binding.step_id,
                        attempt,
                        binding.workspace_path,
                        binding.head_sha,
                        binding.base_sha,
                        binding.payload_digest,
                        Utc::now().to_rfc3339(),
                    ],
                )
                .map_err(|error| authority_error("record rebase recovery certificate", error))?;
            Ok(())
        })
    }

    /// Whether `checkpoint` is exactly the current evidence this host
    /// certified for `run_id` / `step_id`.
    ///
    /// Current means no later attempt of the same step is certified: an
    /// earlier attempt's record stays intact, but it no longer vouches for
    /// anything once a newer recovery completed. A payload without an attempt
    /// predates attempts and is checked against the legacy record, under the
    /// same rule. An absent record means the checkpoint predates the authority
    /// boundary or was authored by something other than the host; both are
    /// unusable, and neither is blessed here.
    pub(crate) fn verify(
        &self,
        run_id: &str,
        step_id: &str,
        checkpoint: &Value,
    ) -> Result<bool, OrbitError> {
        let Ok(binding) = CertificateBinding::derive(run_id, step_id, checkpoint) else {
            return Ok(false);
        };
        let latest = latest_certified_attempt(&self.connection, run_id, step_id)?;
        let recorded = match binding.attempt {
            Some(attempt) if latest == Some(attempt) => {
                certificate(&self.connection, run_id, step_id, attempt)?
            }
            None if latest.is_none() => legacy_certificate(&self.connection, run_id, step_id)?,
            Some(_) | None => return Ok(false),
        };
        Ok(recorded == Some(binding))
    }

    /// Every attempt reserved for `run_id` / `step_id`, oldest first.
    pub(crate) fn attempts(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Vec<RebaseRecoveryAttemptScope>, OrbitError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT workspace_path, head_sha_before, target_base_sha
                 FROM rebase_recovery_attempt
                 WHERE run_id = ?1 AND step_id = ?2
                 ORDER BY attempt",
            )
            .map_err(|error| authority_error("read rebase recovery attempts", error))?;
        let rows = statement
            .query_map(params![run_id, step_id], |row| {
                Ok(RebaseRecoveryAttemptScope {
                    workspace_path: row.get(0)?,
                    head_sha_before: row.get(1)?,
                    target_base_sha: row.get(2)?,
                })
            })
            .map_err(|error| authority_error("read rebase recovery attempts", error))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| authority_error("read rebase recovery attempts", error))
    }

    /// Run `body` in one `BEGIN IMMEDIATE` transaction, so a check and the
    /// write it guards are atomic against every other host connection.
    fn immediate<T>(
        &self,
        action: &str,
        body: impl FnOnce(&Connection) -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(|error| authority_error(action, error))?;
        let outcome = body(&self.connection).and_then(|value| {
            self.connection
                .execute_batch("COMMIT")
                .map_err(|error| authority_error(action, error))
                .map(|()| value)
        });
        if outcome.is_err() && !self.connection.is_autocommit() {
            // The refusal or write failure is what the caller needs; a failed
            // rollback leaves nothing committed either way.
            let _ = self.connection.execute_batch("ROLLBACK");
        }
        outcome
    }
}

fn latest_reservation(
    connection: &Connection,
    run_id: &str,
    step_id: &str,
) -> Result<Option<i64>, OrbitError> {
    connection
        .query_row(
            "SELECT MAX(attempt) FROM rebase_recovery_attempt WHERE run_id = ?1 AND step_id = ?2",
            params![run_id, step_id],
            |row| row.get(0),
        )
        .map_err(|error| authority_error("read rebase recovery attempts", error))
}

fn latest_certified_attempt(
    connection: &Connection,
    run_id: &str,
    step_id: &str,
) -> Result<Option<i64>, OrbitError> {
    connection
        .query_row(
            "SELECT MAX(attempt) FROM rebase_recovery_attempt_certificate
             WHERE run_id = ?1 AND step_id = ?2",
            params![run_id, step_id],
            |row| row.get(0),
        )
        .map_err(|error| authority_error("read rebase recovery certificates", error))
}

fn reservation(
    connection: &Connection,
    run_id: &str,
    step_id: &str,
    attempt: i64,
) -> Result<Option<RebaseRecoveryAttemptScope>, OrbitError> {
    connection
        .query_row(
            "SELECT workspace_path, head_sha_before, target_base_sha
             FROM rebase_recovery_attempt
             WHERE run_id = ?1 AND step_id = ?2 AND attempt = ?3",
            params![run_id, step_id, attempt],
            |row| {
                Ok(RebaseRecoveryAttemptScope {
                    workspace_path: row.get(0)?,
                    head_sha_before: row.get(1)?,
                    target_base_sha: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(|error| authority_error("read rebase recovery attempt", error))
}

fn certificate(
    connection: &Connection,
    run_id: &str,
    step_id: &str,
    attempt: i64,
) -> Result<Option<CertificateBinding>, OrbitError> {
    connection
        .query_row(
            "SELECT workspace_path, head_sha, base_sha, payload_digest
             FROM rebase_recovery_attempt_certificate
             WHERE run_id = ?1 AND step_id = ?2 AND attempt = ?3",
            params![run_id, step_id, attempt],
            |row| {
                Ok(CertificateBinding {
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

/// The pre-attempt record for `run_id` / `step_id`, read for payloads that
/// carry no attempt.
fn legacy_certificate(
    connection: &Connection,
    run_id: &str,
    step_id: &str,
) -> Result<Option<CertificateBinding>, OrbitError> {
    connection
        .query_row(
            "SELECT workspace_path, head_sha, base_sha, payload_digest
             FROM rebase_recovery_certificate
             WHERE run_id = ?1 AND step_id = ?2",
            params![run_id, step_id],
            |row| {
                Ok(CertificateBinding {
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

impl CertificateBinding {
    fn derive(run_id: &str, step_id: &str, checkpoint: &Value) -> Result<Self, OrbitError> {
        // The payload names its own run and step. Certifying a payload under a
        // different identity would leave the two disagreeing, so refuse it at
        // the point the record is derived instead of storing the mismatch.
        let payload_run_id = bound_field(checkpoint, "run_id")?;
        let payload_step_id = bound_field(checkpoint, "step_id")?;
        if payload_run_id != run_id || payload_step_id != step_id {
            return Err(OrbitError::Execution(format!(
                "rebase recovery evidence names run {payload_run_id} step `{payload_step_id}`, \
                 not run {run_id} step `{step_id}`"
            )));
        }

        Ok(Self {
            run_id: run_id.to_string(),
            step_id: step_id.to_string(),
            attempt: recovery_attempt(checkpoint)?,
            workspace_path: bound_field(checkpoint, "workspace_path")?,
            head_sha: bound_field(checkpoint, "head_sha")?,
            base_sha: bound_field(checkpoint, "base_sha")?,
            payload_digest: payload_digest(checkpoint),
        })
    }
}

/// The payload's `recovery_attempt`: absent for a pre-attempt payload, and
/// otherwise a positive integer the host reserved.
fn recovery_attempt(checkpoint: &Value) -> Result<Option<i64>, OrbitError> {
    let Some(value) = checkpoint.get(RECOVERY_ATTEMPT_FIELD) else {
        return Ok(None);
    };
    value
        .as_i64()
        .filter(|attempt| *attempt >= 1)
        .map(Some)
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "rebase recovery evidence has an invalid `{RECOVERY_ATTEMPT_FIELD}`"
            ))
        })
}

fn bound_field(checkpoint: &Value, field: &str) -> Result<String, OrbitError> {
    checkpoint
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "rebase recovery evidence is missing its `{field}` binding"
            ))
        })
}

/// BLAKE3 over a key-sorted rendering of the whole payload, so any edit to any
/// field — including ones the bound columns do not name, such as `task_ids` or
/// `rewritten` — changes the digest.
// Shared with the legacy-certificate security fixture to seed the persisted format.
pub(super) fn payload_digest(checkpoint: &Value) -> String {
    blake3::hash(canonical_json(checkpoint).as_bytes())
        .to_hex()
        .to_string()
}

/// Render `value` with object keys in a stable order. `serde_json` map ordering
/// depends on a feature flag and on how a value was built, and this digest has
/// to survive a round trip through the run store either way.
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(fields) => {
            let sorted = fields
                .iter()
                .map(|(key, value)| (key.as_str(), canonical_json(value)))
                .collect::<BTreeMap<_, _>>();
            let rendered = sorted
                .into_iter()
                .map(|(key, value)| format!("{}:{value}", Value::String(key.to_string())))
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{rendered}}}")
        }
        Value::Array(items) => {
            let rendered = items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{rendered}]")
        }
        other => other.to_string(),
    }
}
