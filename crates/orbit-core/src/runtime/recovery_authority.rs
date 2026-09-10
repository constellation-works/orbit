//! Host-only authority for rebase-recovery evidence.
//!
//! A managed leaf is granted modify access to the shared run store
//! (`<global>/orbit.db` and its WAL/SHM sidecars) so that `orbit.task.*` and
//! audit tools keep working inside the sandbox. That grant also covers the run
//! row holding `PipelineState::rebase_recovery_checkpoints`, so those bytes are
//! progress data, not authority: a leaf can rewrite them.
//!
//! This module owns the separate durable record that *is* authority. It lives
//! in its own SQLite database under a root that appears in no leaf write grant,
//! and [`append_recovery_authority_denies`] appends an explicit deny for that
//! root after every convenience grant, the same shape
//! [`crate::runtime::git_sandbox`] uses for Git metadata.
//!
//! Confinement here is by *location*, not by a secret. Bubblewrap mounts the
//! host filesystem `--ro-bind / /` and enforces only write boundaries, so a key
//! file would be readable by every leaf and a keyed MAC would buy nothing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::policy::ResolvedFsProfile;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

/// Directory holding the authority database, relative to the global root.
const AUTHORITY_DIR: &str = "state/recovery-authority";

/// Database file name. Its `-wal` and `-shm` sidecars are created beside it and
/// are covered by the same protected root.
const AUTHORITY_DB: &str = "authority.db";

/// Owner-only permissions for the authority root and its database. These do not
/// confine a same-UID leaf on their own; they keep the record off any shared or
/// group-readable path.
#[cfg(unix)]
const AUTHORITY_DIR_MODE: u32 = 0o700;
#[cfg(unix)]
const AUTHORITY_FILE_MODE: u32 = 0o600;

/// The durable record a host writes when it completes a rebase recovery, and
/// the only evidence a later resume will accept.
pub(crate) struct RecoveryAuthority {
    connection: Connection,
}

/// The identity a certificate binds. Every field is re-derived from the
/// candidate checkpoint at verification time, so a payload that was edited,
/// moved to another run or step, or pointed at another workspace no longer
/// matches the record the host wrote.
#[derive(Debug, PartialEq, Eq)]
struct CertificateBinding {
    run_id: String,
    step_id: String,
    workspace_path: String,
    head_sha: String,
    base_sha: String,
    payload_digest: String,
}

impl RecoveryAuthority {
    /// Open (creating on first use) the authority for `global_root`.
    pub(crate) fn open(global_root: &Path) -> Result<Self, OrbitError> {
        let root = authority_root(global_root)?;
        let connection = Connection::open(root.join(AUTHORITY_DB))
            .map_err(|error| authority_error("open recovery authority database", error))?;

        // WAL keeps the sidecars inside the protected root rather than beside
        // the shared store, and matches how every other Orbit database runs.
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|error| authority_error("enable WAL on recovery authority", error))?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(|error| authority_error("harden recovery authority durability", error))?;
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
                 ) WITHOUT ROWID;",
            )
            .map_err(|error| authority_error("create recovery authority schema", error))?;
        restrict_permissions(&root)?;

        Ok(Self { connection })
    }

    /// Record the host's completion of `step_id` in `run_id`.
    ///
    /// Certificates are immutable: re-issuing the identical payload succeeds so
    /// a retried write is harmless, while a different payload for an already
    /// certified step is refused rather than replacing the accepted record.
    pub(crate) fn issue(
        &self,
        run_id: &str,
        step_id: &str,
        checkpoint: &Value,
    ) -> Result<(), OrbitError> {
        let binding = CertificateBinding::derive(run_id, step_id, checkpoint)?;

        if let Some(recorded) = self.read(run_id, step_id)? {
            if recorded == binding {
                return Ok(());
            }
            return Err(OrbitError::Execution(format!(
                "rebase recovery for run {run_id} step `{step_id}` is already certified with \
                 different evidence; the accepted certificate is immutable"
            )));
        }

        self.connection
            .execute(
                "INSERT INTO rebase_recovery_certificate
                     (run_id, step_id, workspace_path, head_sha, base_sha, payload_digest, issued_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    binding.run_id,
                    binding.step_id,
                    binding.workspace_path,
                    binding.head_sha,
                    binding.base_sha,
                    binding.payload_digest,
                    Utc::now().to_rfc3339(),
                ],
            )
            .map_err(|error| authority_error("record rebase recovery certificate", error))?;
        Ok(())
    }

    /// Whether `checkpoint` is exactly the evidence this host certified for
    /// `run_id` / `step_id`. An absent record means the checkpoint predates the
    /// authority boundary or was authored by something other than the host;
    /// both are unusable, and neither is blessed here.
    pub(crate) fn verify(
        &self,
        run_id: &str,
        step_id: &str,
        checkpoint: &Value,
    ) -> Result<bool, OrbitError> {
        let Ok(binding) = CertificateBinding::derive(run_id, step_id, checkpoint) else {
            return Ok(false);
        };
        Ok(self.read(run_id, step_id)? == Some(binding))
    }

    fn read(&self, run_id: &str, step_id: &str) -> Result<Option<CertificateBinding>, OrbitError> {
        self.connection
            .query_row(
                "SELECT workspace_path, head_sha, base_sha, payload_digest
                 FROM rebase_recovery_certificate
                 WHERE run_id = ?1 AND step_id = ?2",
                params![run_id, step_id],
                |row| {
                    Ok(CertificateBinding {
                        run_id: run_id.to_string(),
                        step_id: step_id.to_string(),
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
            workspace_path: bound_field(checkpoint, "workspace_path")?,
            head_sha: bound_field(checkpoint, "head_sha")?,
            base_sha: bound_field(checkpoint, "base_sha")?,
            payload_digest: payload_digest(checkpoint),
        })
    }
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
fn payload_digest(checkpoint: &Value) -> String {
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

/// The protected root, created and validated on demand.
///
/// A symlink anywhere in the path would let a writable location stand in for
/// the authority, so refuse that layout outright rather than deny a pointer
/// that can be replaced.
fn authority_root(global_root: &Path) -> Result<PathBuf, OrbitError> {
    let root = global_root.join(AUTHORITY_DIR);
    fs::create_dir_all(&root)
        .map_err(|error| path_error("create recovery authority root", &root, error))?;
    let root = root
        .canonicalize()
        .map_err(|error| path_error("canonicalize recovery authority root", &root, error))?;
    refuse_symlinked_path(&root)?;
    Ok(root)
}

fn refuse_symlinked_path(path: &Path) -> Result<(), OrbitError> {
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)
            .map_err(|error| path_error("inspect recovery authority path", ancestor, error))?;
        if metadata.file_type().is_symlink() {
            return Err(OrbitError::PolicyDenied(format!(
                "recovery authority refuses symlinked path `{}`",
                ancestor.display()
            )));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(root: &Path) -> Result<(), OrbitError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(root, fs::Permissions::from_mode(AUTHORITY_DIR_MODE))
        .map_err(|error| path_error("restrict recovery authority root", root, error))?;
    for name in [
        AUTHORITY_DB.to_string(),
        format!("{AUTHORITY_DB}-wal"),
        format!("{AUTHORITY_DB}-shm"),
    ] {
        let file = root.join(name);
        if file.exists() {
            fs::set_permissions(&file, fs::Permissions::from_mode(AUTHORITY_FILE_MODE))
                .map_err(|error| path_error("restrict recovery authority file", &file, error))?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_root: &Path) -> Result<(), OrbitError> {
    Ok(())
}

/// Deny the authority root to a sandboxed leaf.
///
/// No convenience grant names this root today, so this is a tripwire rather
/// than the only barrier: appended after every other grant, it keeps a future
/// broadening of `<global>` grants from silently reopening the store. The
/// subtree form covers `authority.db` together with its `-wal` and `-shm`
/// sidecars, and Bubblewrap binds each writable ancestor of a deny so the
/// directory cannot be renamed aside and replaced.
pub(crate) fn append_recovery_authority_denies(
    global_root: &Path,
    resolved: &mut ResolvedFsProfile,
) -> Result<(), OrbitError> {
    let root = authority_root(global_root)?;
    let deny = format!("!{}/**", root.display());
    if !resolved.modify.iter().any(|rule| rule == &deny) {
        resolved.modify.push(deny);
    }
    Ok(())
}

fn authority_error(action: &str, error: rusqlite::Error) -> OrbitError {
    OrbitError::Execution(format!("{action}: {error}"))
}

fn path_error(action: &str, path: &Path, error: std::io::Error) -> OrbitError {
    OrbitError::Execution(format!("{action} `{}`: {error}", path.display()))
}

#[cfg(test)]
#[path = "tests/recovery_authority.rs"]
mod tests;
