//! `workflow.lock_modes`: the ordered per-path footprint lock modes owner
//! admission applies to selector overlaps.

use orbit_common::OrbitError;
use orbit_common::fs::lock_mode::{FootprintLockMode, FootprintLockModeEntry, FootprintLockModes};
use serde::Deserialize;

/// The key as written in `config.toml`.
pub const LOCK_MODES_KEY: &str = "workflow.lock_modes";

/// One `[[workflow.lock_modes]]` entry as written.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawLockModeEntry {
    #[serde(default)]
    pattern: String,
    #[serde(default)]
    mode: Option<String>,
}

/// Admit `workflow.lock_modes`: ordered entries, each a non-empty regex and a
/// mode of `exclusive` (the default) or `shared`. An error names the entry by
/// its 1-based position and pattern. Unset or empty locks every overlap.
pub(crate) fn admit_lock_modes(
    raw: Option<Vec<RawLockModeEntry>>,
) -> Result<Vec<FootprintLockModeEntry>, OrbitError> {
    let invalid = |reason: String| OrbitError::InvalidInput(format!("{LOCK_MODES_KEY} {reason}"));
    let mut admitted = Vec::new();
    for (index, entry) in raw.unwrap_or_default().into_iter().enumerate() {
        let label = format!("entry {} (pattern `{}`)", index + 1, entry.pattern);
        if entry.pattern.trim().is_empty() {
            return Err(invalid(format!("entry {} has an empty pattern", index + 1)));
        }
        let mode = match entry.mode.as_deref() {
            None => FootprintLockMode::Exclusive,
            Some(mode) => FootprintLockMode::parse(mode).ok_or_else(|| {
                invalid(format!(
                    "{label} has unknown mode `{mode}`; expected one of {}",
                    FootprintLockMode::CHOICES.join(", ")
                ))
            })?,
        };
        admitted.push(FootprintLockModeEntry {
            pattern: entry.pattern,
            mode,
        });
    }
    FootprintLockModes::new(&admitted).map_err(invalid)?;
    Ok(admitted)
}
