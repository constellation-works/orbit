//! JSONL sidecars (events, comments): whole-file writes, reads, durable
//! appends and torn-tail repair.

use crate::driver::file::task_bundle::bundle_io::read_required_text;
use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, with_exclusive_file_lock};
use orbit_types::task::{TaskCommentRowV2, TaskEventRowV2};
use serde::de::DeserializeOwned;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub(super) fn write_jsonl_file<T>(path: &Path, rows: &[T]) -> Result<(), OrbitError>
where
    T: serde::Serialize,
{
    let mut content = String::new();
    for row in rows {
        content.push_str(
            &serde_json::to_string(row).map_err(|err| OrbitError::Store(err.to_string()))?,
        );
        content.push('\n');
    }
    atomic_write_text(path, &content).map_err(|err| OrbitError::from_write_io(path, err))
}

pub(super) fn read_task_events(path: &Path) -> Result<Vec<TaskEventRowV2>, OrbitError> {
    let events: Vec<TaskEventRowV2> = read_jsonl_file(path)?;
    for event in &events {
        event.validate()?;
    }
    Ok(events)
}

pub(super) fn read_task_comments(path: &Path) -> Result<Vec<TaskCommentRowV2>, OrbitError> {
    let comments: Vec<TaskCommentRowV2> = read_jsonl_file(path)?;
    for comment in &comments {
        comment.validate()?;
    }
    Ok(comments)
}

fn read_jsonl_file<T>(path: &Path) -> Result<Vec<T>, OrbitError>
where
    T: DeserializeOwned,
{
    let raw = read_required_text(path)?;
    scan_jsonl_records(path, &raw)
}

pub(crate) fn append_jsonl_row<T>(path: &Path, row: &T) -> Result<(), OrbitError>
where
    T: serde::Serialize,
{
    let encoded = serde_json::to_string(row).map_err(|err| OrbitError::Store(err.to_string()))?;
    with_exclusive_file_lock(path, "task jsonl", || {
        repair_jsonl_tail(path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|err| OrbitError::from_write_io(path, err))?;
        file.write_all(encoded.as_bytes())
            .map_err(|err| OrbitError::from_write_io(path, err))?;
        file.write_all(b"\n")
            .map_err(|err| OrbitError::from_write_io(path, err))?;
        file.flush()
            .map_err(|err| OrbitError::from_write_io(path, err))?;
        file.sync_all()
            .map_err(|err| OrbitError::from_write_io(path, err))?;
        Ok(())
    })
}

fn repair_jsonl_tail(path: &Path) -> Result<(), OrbitError> {
    let Ok(mut file) = OpenOptions::new().read(true).write(true).open(path) else {
        return Ok(());
    };

    let mut raw = String::new();
    file.read_to_string(&mut raw).map_err(OrbitError::from)?;
    let scan = scan_jsonl_tail(path, &raw)?;
    if scan.truncate_at < raw.len() as u64 {
        file.set_len(scan.truncate_at)
            .map_err(|err| OrbitError::from_write_io(path, err))?;
        file.seek(SeekFrom::End(0))
            .map_err(|err| OrbitError::from_write_io(path, err))?;
        file.sync_all()
            .map_err(|err| OrbitError::from_write_io(path, err))?;
    }
    Ok(())
}

pub(super) fn scan_jsonl_records<T>(path: &Path, raw: &str) -> Result<Vec<T>, OrbitError>
where
    T: DeserializeOwned,
{
    let scan = scan_jsonl_tail(path, raw)?;
    let valid = &raw[..scan.truncate_at as usize];
    valid
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|err| {
                OrbitError::Store(format!("invalid JSONL row at {}: {err}", path.display()))
            })
        })
        .collect()
}

struct JsonlTailScan {
    truncate_at: u64,
}

fn scan_jsonl_tail(path: &Path, raw: &str) -> Result<JsonlTailScan, OrbitError> {
    if raw.is_empty() {
        return Ok(JsonlTailScan { truncate_at: 0 });
    }

    let mut offset = 0usize;
    let mut last_good = 0usize;
    for chunk in raw.split_inclusive('\n') {
        let next_offset = offset + chunk.len();
        if !chunk.ends_with('\n') {
            return Ok(JsonlTailScan {
                truncate_at: last_good as u64,
            });
        }

        let line = chunk.trim_end_matches('\n').trim_end_matches('\r');
        if line.trim().is_empty() {
            return Err(OrbitError::Store(format!(
                "blank JSONL row before tail at {}",
                path.display()
            )));
        }
        if let Err(err) = serde_json::from_str::<serde_json::Value>(line) {
            if next_offset == raw.len() {
                return Ok(JsonlTailScan {
                    truncate_at: last_good as u64,
                });
            }
            return Err(OrbitError::Store(format!(
                "invalid JSONL row before tail at {}: {err}",
                path.display()
            )));
        }

        last_good = next_offset;
        offset = next_offset;
    }

    Ok(JsonlTailScan {
        truncate_at: last_good as u64,
    })
}
