//! What the workspace scratch sweep reports back to the worktree GC activity.

use std::path::PathBuf;

use serde::Serialize;

/// One top-level entry of the checkout's `.orbit/tmp` that the sweep removed
/// or deliberately left alone. Entries inside the retention window are only
/// counted, not listed.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScratchGcEntry {
    pub path: String,
    /// `removed` or `skipped`.
    pub action: String,
    pub bytes_reclaimed: u64,
    /// Why the entry was skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Outcome of pruning the owning checkout's `.orbit/tmp` by age.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScratchGcReport {
    pub path: PathBuf,
    pub retention_hours: u64,
    pub bytes_reclaimed: u64,
    pub entries_removed: usize,
    /// Entries old enough to remove that were left because a process holds
    /// them, an active run names them, or removal failed.
    pub entries_skipped: usize,
    /// Entries newer than the retention window.
    pub entries_kept: usize,
    pub entries: Vec<ScratchGcEntry>,
}
