//! The clock writer's record of consecutive ticks refused by a live generation.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use super::paths::{CLOCK_HOLD, validated_generation_root};
use super::refusal::{SWITCH_PENDING, WRITES_WHILE_FOREIGN};
use crate::OrbitError;

#[derive(Serialize, Deserialize)]
struct ClockHold {
    digest: String,
    started_at: DateTime<Utc>,
    last_refused_at: DateTime<Utc>,
    refused_ticks: u64,
}

/// True only for a clock writer held back by other live generations: a
/// still-pinned v1 generation, or a pending switch it is not part of.
pub fn is_clock_generation_hold(error: &OrbitError) -> bool {
    let message = error.to_string();
    message.contains(WRITES_WHILE_FOREIGN) || message.contains(SWITCH_PENDING)
}

/// Persist consecutive refused ticks without emitting one error per process.
pub fn record_clock_generation_hold(
    root: &Path,
    digest: &str,
    at: DateTime<Utc>,
) -> Result<(), OrbitError> {
    with_clock_hold(root, |hold| {
        match hold {
            Some(active) if active.digest == digest => {
                active.last_refused_at = at;
                active.refused_ticks = active.refused_ticks.saturating_add(1);
            }
            _ => {
                *hold = Some(ClockHold {
                    digest: digest.to_string(),
                    started_at: at,
                    last_refused_at: at,
                    refused_ticks: 1,
                });
            }
        }
        Ok(())
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

fn with_clock_hold<T>(
    root: &Path,
    change: impl FnOnce(&mut Option<ClockHold>) -> Result<T, OrbitError>,
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
    if file
        .metadata()
        .map_err(|error| OrbitError::Io(error.to_string()))?
        .len()
        > 1024
    {
        return Err(OrbitError::InvalidInput(
            "clock hold record exceeds 1024 bytes".into(),
        ));
    }
    let mut raw = String::new();
    file.read_to_string(&mut raw)
        .map_err(|error| OrbitError::Io(format!("read clock hold record: {error}")))?;
    let mut hold = if raw.is_empty() {
        None
    } else {
        Some(serde_json::from_str(&raw).map_err(|error| {
            OrbitError::InvalidInput(format!("invalid clock hold record: {error}"))
        })?)
    };
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
