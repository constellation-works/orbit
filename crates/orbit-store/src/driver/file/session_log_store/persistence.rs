//! Locked JSONL persistence for [`super::SessionLogStore`].
//!
//! Every read and mutation holds `.session-log.jsonl.lock`. While that lock is
//! held, a malformed unterminated final row is truncated so a torn append cannot
//! wedge later list/resolve/append. Newline-terminated corrupt rows still fail.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_bytes, sync_parent_dir};

use super::{SessionLogAppendParams, SessionLogEntry, SessionLogFilter, SessionLogKind};
use crate::fs::lock::acquire_exclusive;

const LOG_FILE_NAME: &str = "session-log.jsonl";
const LOCK_FILE_NAME: &str = ".session-log.jsonl.lock";

pub(super) fn append(
    orbit_dir: &Path,
    params: SessionLogAppendParams,
) -> Result<SessionLogEntry, OrbitError> {
    let body = params.body.trim().to_string();
    if body.is_empty() {
        return Err(OrbitError::InvalidInput(
            "session-log body must be non-empty".to_string(),
        ));
    }

    let _guard = acquire_exclusive(&lock_path(orbit_dir), "session-log append")?;
    let path = log_path(orbit_dir);
    let entries = read_entries(&path)?;
    let entry = SessionLogEntry {
        id: next_id(&entries)?,
        at: Utc::now(),
        kind: params.kind,
        body,
        related_task_ids: params.related_task_ids,
        related_run_ids: params.related_run_ids,
        resolved_at: None,
    };
    append_entry(&path, &entry)?;
    Ok(entry)
}

pub(super) fn list(
    orbit_dir: &Path,
    filter: &SessionLogFilter,
) -> Result<Vec<SessionLogEntry>, OrbitError> {
    let _guard = acquire_exclusive(&lock_path(orbit_dir), "session-log read")?;
    let mut entries = read_entries(&log_path(orbit_dir))?;
    entries.retain(|entry| {
        filter.kind.is_none_or(|want| entry.kind == want)
            && filter.since.is_none_or(|bound| entry.at >= bound)
            && (!filter.unresolved_only
                || (entry.kind == SessionLogKind::CheckLater && entry.resolved_at.is_none()))
    });
    Ok(entries)
}

pub(super) fn resolve(orbit_dir: &Path, id: &str) -> Result<SessionLogEntry, OrbitError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(OrbitError::InvalidInput(
            "session-log id is required".to_string(),
        ));
    }

    let _guard = acquire_exclusive(&lock_path(orbit_dir), "session-log resolve")?;
    let path = log_path(orbit_dir);
    let mut entries = read_entries(&path)?;
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
    replace_entries(&path, &entries)?;
    Ok(entries[index].clone())
}

fn log_path(orbit_dir: &Path) -> PathBuf {
    orbit_dir.join(LOG_FILE_NAME)
}

fn lock_path(orbit_dir: &Path) -> PathBuf {
    orbit_dir.join(LOCK_FILE_NAME)
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

fn append_entry(path: &Path, entry: &SessionLogEntry) -> Result<(), OrbitError> {
    let mut encoded = serde_json::to_vec(entry)
        .map_err(|error| OrbitError::Execution(format!("serialize session-log entry: {error}")))?;
    encoded.push(b'\n');

    let existed = path.exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| OrbitError::Io(format!("open {}: {error}", path.display())))?;
    file.write_all(&encoded)
        .map_err(|error| OrbitError::Io(format!("append {}: {error}", path.display())))?;
    file.sync_data()
        .map_err(|error| OrbitError::Io(format!("sync {}: {error}", path.display())))?;
    if !existed {
        let parent = path.parent().ok_or_else(|| {
            OrbitError::Io(format!(
                "sync parent for {}: no parent directory",
                path.display()
            ))
        })?;
        let parent_dir = File::open(parent).map_err(|error| {
            OrbitError::Io(format!("sync parent for {}: {error}", path.display()))
        })?;
        sync_parent_dir(&parent_dir).map_err(|error| {
            OrbitError::Io(format!("sync parent for {}: {error}", path.display()))
        })?;
    }
    Ok(())
}

fn read_entries(path: &Path) -> Result<Vec<SessionLogEntry>, OrbitError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read(path)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", path.display())))?;
    let scan = scan_entries(path, &raw)?;
    persist_scan_repair(path, raw.len(), &scan)?;
    Ok(scan.entries)
}

struct SessionLogScan {
    entries: Vec<SessionLogEntry>,
    truncate_at: usize,
    missing_newline: bool,
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
    path: &Path,
    raw_len: usize,
    scan: &SessionLogScan,
) -> Result<(), OrbitError> {
    if scan.truncate_at < raw_len {
        truncate_log(path, scan.truncate_at)
    } else if scan.missing_newline {
        append_trailing_newline(path)
    } else {
        Ok(())
    }
}

fn truncate_log(path: &Path, truncate_at: usize) -> Result<(), OrbitError> {
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|error| OrbitError::Io(format!("open {}: {error}", path.display())))?;
    file.set_len(truncate_at as u64)
        .map_err(|error| OrbitError::Io(format!("truncate {}: {error}", path.display())))?;
    file.sync_all()
        .map_err(|error| OrbitError::Io(format!("sync {}: {error}", path.display())))
}

fn append_trailing_newline(path: &Path) -> Result<(), OrbitError> {
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|error| OrbitError::Io(format!("open {}: {error}", path.display())))?;
    file.write_all(b"\n")
        .map_err(|error| OrbitError::Io(format!("append {}: {error}", path.display())))?;
    file.sync_data()
        .map_err(|error| OrbitError::Io(format!("sync {}: {error}", path.display())))
}

fn replace_entries(path: &Path, entries: &[SessionLogEntry]) -> Result<(), OrbitError> {
    let mut encoded = Vec::new();
    for entry in entries {
        serde_json::to_writer(&mut encoded, entry).map_err(|error| {
            OrbitError::Execution(format!("serialize session-log entry: {error}"))
        })?;
        encoded.push(b'\n');
    }
    atomic_write_bytes(path, &encoded)
        .map_err(|error| OrbitError::Io(format!("replace {}: {error}", path.display())))
}
