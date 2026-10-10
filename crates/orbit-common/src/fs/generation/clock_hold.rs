//! The clock writer's record of consecutive ticks refused by upgrade admission.
//!
//! Two refusals are recorded. A tick held back by other live generations (a
//! still-pinned v1 generation, or a pending switch it is not part of) is
//! routine and stays quiet until it resumes. A tick refused outright because
//! a breaking migration cannot run beside the live processes stops routines
//! and worktree GC until they exit, so the record keeps its reason for
//! `orbit doctor` and every refused tick says so.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::paths::{CLOCK_HOLD, validated_generation_record_path, validated_generation_root};
use super::refusal::{BREAKING_WAITING, INCOMPATIBLE, SWITCH_PENDING, WRITES_WHILE_FOREIGN};
use crate::OrbitError;

/// The record never exceeds this many bytes; a longer reason is cut short.
const CLOCK_HOLD_MAX_BYTES: u64 = 1024;
const REFUSAL_MAX_BYTES: usize = 640;

/// Consecutive clock ticks refused by upgrade admission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClockGenerationHold {
    /// Digest of the refused clock executable.
    pub digest: String,
    /// First refused tick.
    pub started_at: DateTime<Utc>,
    /// Latest refused tick.
    pub last_refused_at: DateTime<Utc>,
    /// Ticks refused in a row.
    pub refused_ticks: u64,
    /// Why admission refused the latest tick outright; absent while the tick
    /// was only held back behind another live generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
}

/// True only for a clock writer held back by other live generations: a
/// still-pinned v1 generation, or a pending switch it is not part of.
pub fn is_clock_generation_hold(error: &OrbitError) -> bool {
    let message = error.to_string();
    message.contains(WRITES_WHILE_FOREIGN) || message.contains(SWITCH_PENDING)
}

/// True for a clock writer refused because a breaking migration cannot run
/// beside the live processes: it waited for them to yield, or it is older.
pub fn is_clock_upgrade_refusal(error: &OrbitError) -> bool {
    let message = error.to_string();
    message.contains(BREAKING_WAITING) || message.contains(INCOMPATIBLE)
}

/// Persist consecutive refused ticks without emitting one error per process.
pub fn record_clock_generation_hold(
    root: &Path,
    digest: &str,
    at: DateTime<Utc>,
) -> Result<(), OrbitError> {
    record(root, digest, at, None).map(drop)
}

/// Persist a tick refused outright, with its reason, and return the run of
/// refused ticks it extends.
pub fn record_clock_upgrade_refusal(
    root: &Path,
    digest: &str,
    at: DateTime<Utc>,
    error: &OrbitError,
) -> Result<ClockGenerationHold, OrbitError> {
    record(root, digest, at, Some(bounded(error.to_string())))
}

fn record(
    root: &Path,
    digest: &str,
    at: DateTime<Utc>,
    refusal: Option<String>,
) -> Result<ClockGenerationHold, OrbitError> {
    with_clock_hold(root, |hold| {
        match hold {
            Some(active) if active.digest == digest => {
                active.last_refused_at = at;
                active.refused_ticks = active.refused_ticks.saturating_add(1);
                active.refusal = refusal;
            }
            _ => {
                *hold = Some(ClockGenerationHold {
                    digest: digest.to_string(),
                    started_at: at,
                    last_refused_at: at,
                    refused_ticks: 1,
                    refusal,
                });
            }
        }
        hold.clone()
            .ok_or_else(|| OrbitError::Execution("clock hold record was not kept".into()))
    })
}

/// Return one dated summary when the refused generation can run again.
pub fn finish_clock_generation_hold(
    root: &Path,
    digest: &str,
    at: DateTime<Utc>,
) -> Result<Option<String>, OrbitError> {
    with_clock_hold(root, |hold| {
        let Some(active) = hold.as_ref().filter(|active| active.digest == digest) else {
            return Ok(None);
        };
        let summary = format!(
            "clock executable generation changed under live Orbit processes (possibly drain workers): started_at={} ended_at={} last_refused_at={} refused_ticks={}",
            active.started_at.to_rfc3339(),
            at.to_rfc3339(),
            active.last_refused_at.to_rfc3339(),
            active.refused_ticks,
        );
        *hold = None;
        Ok(Some(summary))
    })
}

/// The ticks refused since the clock last ran, read without creating or
/// changing the record.
pub fn clock_generation_hold(root: &Path) -> Result<Option<ClockGenerationHold>, OrbitError> {
    let path = validated_generation_record_path(root, CLOCK_HOLD)?;
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(OrbitError::Io(format!("open clock hold record: {error}"))),
    };
    FileExt::lock_shared(&file)
        .map_err(|error| OrbitError::Io(format!("lock clock hold record: {error}")))?;
    decode(&file)
}

/// Cut `reason` to [`REFUSAL_MAX_BYTES`] on a character boundary.
fn bounded(mut reason: String) -> String {
    if reason.len() > REFUSAL_MAX_BYTES {
        let mut end = REFUSAL_MAX_BYTES;
        while !reason.is_char_boundary(end) {
            end -= 1;
        }
        reason.truncate(end);
        reason.push('…');
    }
    reason
}

fn decode(mut file: &File) -> Result<Option<ClockGenerationHold>, OrbitError> {
    if file
        .metadata()
        .map_err(|error| OrbitError::Io(error.to_string()))?
        .len()
        > CLOCK_HOLD_MAX_BYTES
    {
        return Err(OrbitError::InvalidInput(format!(
            "clock hold record exceeds {CLOCK_HOLD_MAX_BYTES} bytes"
        )));
    }
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .map_err(|error| OrbitError::Io(format!("read clock hold record: {error}")))?;
    if raw.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|error| OrbitError::InvalidInput(format!("invalid clock hold record: {error}")))
}

fn with_clock_hold<T>(
    root: &Path,
    change: impl FnOnce(&mut Option<ClockGenerationHold>) -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    let path = validated_generation_root(root)?.join(CLOCK_HOLD);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| OrbitError::Io(format!("open clock hold record: {error}")))?;
    FileExt::lock_exclusive(&file)
        .map_err(|error| OrbitError::Io(format!("lock clock hold record: {error}")))?;
    let mut hold = decode(&file)?;
    let result = change(&mut hold)?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| OrbitError::Io(error.to_string()))?;
    let encoded = match hold {
        Some(hold) => serde_json::to_vec(&hold)
            .map_err(|error| OrbitError::Execution(format!("encode clock hold record: {error}")))?,
        None => Vec::new(),
    };
    file.write_all(&encoded)
        .map_err(|error| OrbitError::Io(format!("write clock hold record: {error}")))?;
    file.set_len(encoded.len() as u64)
        .map_err(|error| OrbitError::Io(format!("truncate clock hold record: {error}")))?;
    file.sync_data()
        .map_err(|error| OrbitError::Io(format!("sync clock hold record: {error}")))?;
    Ok(result)
}
