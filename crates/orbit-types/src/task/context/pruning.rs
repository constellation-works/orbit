//! Typed view of the retired filesystem-existence pruning history record.
//!
//! Nothing emits [`CONTEXT_FILES_PRUNED_EVENT`] any more — a declared
//! selector whose target is missing is kept — but stored histories still
//! carry these entries, and they are the only authoritative record of what
//! a task declared before its scope was pruned. Restoration and forecast
//! reconstruction read them through [`LegacyContextPruningRecord`] so they
//! cite evidence instead of inventing scope.
//!
//! This is a read-only decoded view, not a new serialized history or event
//! shape: the only decoder reads the historical note text directly, and no
//! writer for this event remains.

use crate::task::TaskHistoryEntry;

/// History event written by the retired filesystem-existence pruning path.
pub const CONTEXT_FILES_PRUNED_EVENT: &str = "context_files_pruned";

/// Prefix and suffix of a [`CONTEXT_FILES_PRUNED_EVENT`] note, which is
/// `dropped: <selector>, <selector> (selector anchor not found in workspace)`.
const PRUNED_NOTE_PREFIX: &str = "dropped: ";
const PRUNED_NOTE_SUFFIX: &str = " (selector anchor not found in workspace)";

/// The selectors one legacy pruning history entry recorded as dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyContextPruningRecord {
    /// Selectors the pruning entry recorded as dropped, in recorded order.
    pub selectors: Vec<String>,
}

impl LegacyContextPruningRecord {
    /// Decode the pruning record one history entry carries.
    ///
    /// `None` when the entry is not a [`CONTEXT_FILES_PRUNED_EVENT`] entry,
    /// carries no note, or its note does not match the recorded shape: an
    /// unparseable entry is missing evidence, and guessing at its contents
    /// is the scope invention the readers of this record exist to avoid.
    #[must_use]
    pub fn from_entry(entry: &TaskHistoryEntry) -> Option<Self> {
        if entry.event != CONTEXT_FILES_PRUNED_EVENT {
            return None;
        }
        let listed = entry
            .note
            .as_deref()?
            .trim()
            .strip_prefix(PRUNED_NOTE_PREFIX)?;
        Some(Self {
            selectors: listed
                .strip_suffix(PRUNED_NOTE_SUFFIX)
                .unwrap_or(listed)
                .split(',')
                .map(str::trim)
                .filter(|selector| !selector.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
        })
    }
}
