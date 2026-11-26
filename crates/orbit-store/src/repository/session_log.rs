//! Concrete repository for the workspace session log (JSON Lines records).
//!
//! Reached only through [`FileSnapshot::session_log`] (read-only) and
//! [`FileTx::session_log`] (read-write inside [`FileStore::write`]). The file,
//! its lock and its durable writes are `driver/file/session_log_store`'s; this
//! repository owns the record format. Each method holds the lock for its
//! whole read-modify-write. While the lock is held, reads ignore a malformed
//! unterminated final row without changing the log; append repairs a torn
//! tail before writing, and resolution replaces the recovered records.
//! Newline-terminated corrupt rows still fail.
//!
//! [`FileStore::write`]: crate::file_store::FileStore::write

use std::marker::PhantomData;
use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;

use crate::contract::{SessionLogAppendParams, SessionLogEntry, SessionLogFilter, SessionLogKind};
use crate::driver::file::session_log_store::SessionLogDir;
use crate::file_store::{FileSnapshot, FileTx};

/// Concrete session-log repository scoped to a [`FileSnapshot`] (read-only)
/// or a [`FileTx`] (read-write).
pub struct SessionLogRepository<'a, Access> {
    dir: &'a SessionLogDir,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> SessionLogRepository<'a, FileSnapshot<'s, SessionLogDir>> {
    pub(crate) fn for_snapshot(snapshot: &'a FileSnapshot<'s, SessionLogDir>) -> Self {
        Self {
            dir: snapshot.layout(),
            _access: PhantomData,
        }
    }
}

impl<Access> SessionLogRepository<'_, Access> {
    /// Records matching `filter`, from one lock-protected read, without
    /// modifying the log and tolerating a malformed unterminated final row.
    pub fn list(&self, filter: SessionLogFilter) -> Result<Vec<SessionLogEntry>, OrbitError> {
        let _guard = self.dir.lock("session-log read")?;
        let mut entries = read_entries(self.dir)?;
        entries.retain(|entry| {
            filter.kind.is_none_or(|want| entry.kind == want)
                && filter.since.is_none_or(|bound| entry.at >= bound)
                && (!filter.unresolved_only
                    || (entry.kind == SessionLogKind::CheckLater && entry.resolved_at.is_none()))
        });
        Ok(entries)
    }
}

impl<'a, 'tx> SessionLogRepository<'a, FileTx<'tx, SessionLogDir>> {
    pub(crate) fn for_tx(tx: &'a FileTx<'tx, SessionLogDir>) -> Self {
        Self {
            dir: tx.layout(),
            _access: PhantomData,
        }
    }

    /// Append one record and allocate its sequential ID atomically with
    /// respect to every other session-log operation.
    pub fn append(&self, params: SessionLogAppendParams) -> Result<SessionLogEntry, OrbitError> {
        let body = params.body.trim().to_string();
        if body.is_empty() {
            return Err(OrbitError::InvalidInput(
                "session-log body must be non-empty".to_string(),
            ));
        }

        let _guard = self.dir.lock("session-log append")?;
        let entries = read_entries_for_append(self.dir)?;
        let entry = SessionLogEntry {
            id: next_id(&entries)?,
            at: Utc::now(),
            kind: params.kind,
            body,
            related_task_ids: params.related_task_ids,
            related_run_ids: params.related_run_ids,
            resolved_at: None,
        };
        let mut encoded = serde_json::to_vec(&entry).map_err(|error| {
            OrbitError::Execution(format!("serialize session-log entry: {error}"))
        })?;
        encoded.push(b'\n');
        self.dir.append(&encoded)?;
        Ok(entry)
    }

    /// Mark one unresolved `check_later` record resolved using a crash-safe
    /// replacement.
    pub fn resolve(&self, id: &str) -> Result<SessionLogEntry, OrbitError> {
        let id = id.trim();
        if id.is_empty() {
            return Err(OrbitError::InvalidInput(
                "session-log id is required".to_string(),
            ));
        }

        let _guard = self.dir.lock("session-log resolve")?;
        let mut entries = read_entries(self.dir)?;
        let Some(index) = entries.iter().position(|entry| entry.id == id) else {
            return Err(OrbitError::InvalidInput(format!(
                "session-log id not found: {id}"
            )));
        };
        {
            let entry = &mut entries[index];
            if entry.kind != SessionLogKind::CheckLater {
                return Err(OrbitError::InvalidInput(format!(
                    "{} is kind {:?} — only check_later entries can be resolved",
                    entry.id, entry.kind
                )));
            }
            if entry.resolved_at.is_some() {
                return Err(OrbitError::InvalidInput(format!(
                    "{} is already resolved",
                    entry.id
                )));
            }
            entry.resolved_at = Some(Utc::now());
        }
        let mut encoded = Vec::new();
        for entry in &entries {
            serde_json::to_writer(&mut encoded, entry).map_err(|error| {
                OrbitError::Execution(format!("serialize session-log entry: {error}"))
            })?;
            encoded.push(b'\n');
        }
        self.dir.replace(&encoded)?;
        Ok(entries[index].clone())
    }
}

struct SessionLogScan {
    entries: Vec<SessionLogEntry>,
    truncate_at: usize,
    missing_newline: bool,
}

fn next_id(entries: &[SessionLogEntry]) -> Result<String, OrbitError> {
    let current = entries
        .iter()
        .filter_map(|entry| {
            entry
                .id
                .strip_prefix("SL-")
                .and_then(|digits| digits.parse::<u64>().ok())
        })
        .max()
        .unwrap_or(0);
    let next = current.checked_add(1).ok_or_else(|| {
        OrbitError::Store("session-log sequential ID space is exhausted".to_string())
    })?;
    Ok(format!("SL-{next:04}"))
}

fn read_entries(dir: &SessionLogDir) -> Result<Vec<SessionLogEntry>, OrbitError> {
    let raw = dir.read()?;
    Ok(scan_entries(&dir.log_path(), &raw)?.entries)
}

// Only append needs an in-place repair: resolve writes a complete replacement.
// Both callers hold the session-log lock throughout scanning and mutation.
fn read_entries_for_append(dir: &SessionLogDir) -> Result<Vec<SessionLogEntry>, OrbitError> {
    let raw = dir.read()?;
    let scan = scan_entries(&dir.log_path(), &raw)?;
    persist_scan_repair(dir, raw.len(), &scan)?;
    Ok(scan.entries)
}

fn scan_entries(path: &Path, raw: &[u8]) -> Result<SessionLogScan, OrbitError> {
    let mut entries = Vec::new();
    let mut last_good = 0usize;
    let mut offset = 0usize;
    for (line_number, chunk) in raw.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let next_offset = offset + chunk.len();
        let terminated = chunk.ends_with(b"\n");
        let line_bytes = trim_jsonl_terminator(chunk);
        if line_bytes.iter().all(u8::is_ascii_whitespace) {
            if terminated {
                last_good = next_offset;
                offset = next_offset;
                continue;
            }
            return Ok(recovered_scan(entries, last_good));
        }

        let line = match std::str::from_utf8(line_bytes) {
            Ok(line) => line,
            Err(_) if !terminated => return Ok(recovered_scan(entries, last_good)),
            Err(error) => return Err(parse_error(path, line_number + 1, error)),
        };

        match serde_json::from_str::<SessionLogEntry>(line) {
            Ok(entry) => {
                entries.push(entry);
                last_good = next_offset;
                offset = next_offset;
            }
            Err(_) if !terminated => return Ok(recovered_scan(entries, last_good)),
            Err(error) => return Err(parse_error(path, line_number + 1, error)),
        }
    }

    Ok(SessionLogScan {
        entries,
        truncate_at: last_good,
        missing_newline: !raw.is_empty() && !raw.ends_with(b"\n"),
    })
}

fn recovered_scan(entries: Vec<SessionLogEntry>, truncate_at: usize) -> SessionLogScan {
    SessionLogScan {
        entries,
        truncate_at,
        missing_newline: false,
    }
}

fn trim_jsonl_terminator(chunk: &[u8]) -> &[u8] {
    let without_lf = chunk.strip_suffix(b"\n").unwrap_or(chunk);
    without_lf.strip_suffix(b"\r").unwrap_or(without_lf)
}

fn parse_error(path: &Path, line_number: usize, error: impl std::fmt::Display) -> OrbitError {
    OrbitError::Execution(format!(
        "parse {} line {}: {error}",
        path.display(),
        line_number
    ))
}

fn persist_scan_repair(
    dir: &SessionLogDir,
    raw_len: usize,
    scan: &SessionLogScan,
) -> Result<(), OrbitError> {
    if scan.truncate_at < raw_len {
        dir.truncate(scan.truncate_at)
    } else if scan.missing_newline {
        dir.append_newline()
    } else {
        Ok(())
    }
}
