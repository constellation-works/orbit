//! Freshness-stamped reuse of hydrated task rows.
//!
//! A task-list page hydrates each selected row from its whole bundle: a
//! lock, several file reads and their parses. The dashboard re-fetches the
//! same page on every refresh tick, so most of those bundles are unchanged
//! since the previous page. This cache keeps the hydrated [`TaskRow`] of a
//! bundle together with the freshness stamp that vouched for it (see
//! `bundle_stamp::task_bundle_freshness_stamp`); a row is served again only
//! when the settled bundle directory still reports that stamp.
//!
//! The stamp is taken *before* the bundle read it labels, so a write that
//! races the read can only cost an extra read on the next page: the stored
//! stamp describes the bundle at or before the read, never after it. A
//! bundle that is not settled, or whose stamp cannot be proved, is never
//! served from here and is read exactly as an uncached listing reads it.
//!
//! Entries are bounded by a fixed cap, independent of how many tasks the
//! workspace holds, so a long-lived server's memory does not grow with the
//! store. Eviction is oldest-first; an evicted row is simply re-read.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};

use orbit_types::task::TaskEnvelopeV2;

use crate::contracts::TaskRow;

/// Fixed cap on hydrated rows held at once. A default page is 50 rows, so
/// this holds several pages of distinct filters without growing with the
/// number of registered tasks.
pub(super) const MAX_CACHED_TASK_ROWS: usize = 512;

#[derive(Default)]
pub(super) struct RowCache {
    entries: Mutex<RowCacheEntries>,
}

#[derive(Default)]
struct RowCacheEntries {
    by_id: HashMap<String, CachedRow>,
    /// Insertion order of `by_id` keys, oldest first. May hold a key whose
    /// entry was replaced; eviction skips keys no longer present.
    order: VecDeque<String>,
}

struct CachedRow {
    stamp: String,
    envelope: TaskEnvelopeV2,
    row: TaskRow,
}

impl RowCache {
    /// The row remembered for `task_id` when it was stored under `stamp`,
    /// together with the envelope that bundle carried. The caller still
    /// compares that envelope against the selected candidate, exactly as it
    /// compares a freshly read bundle's envelope.
    pub(super) fn cached_row(
        &self,
        task_id: &str,
        stamp: &str,
    ) -> Option<(TaskEnvelopeV2, TaskRow)> {
        let entries = self.entries();
        let cached = entries.by_id.get(task_id)?;
        if cached.stamp != stamp {
            return None;
        }
        Some((cached.envelope.clone(), cached.row.clone()))
    }

    pub(super) fn remember(
        &self,
        task_id: &str,
        stamp: String,
        envelope: TaskEnvelopeV2,
        row: TaskRow,
    ) {
        let mut entries = self.entries();
        let is_new = !entries.by_id.contains_key(task_id);
        entries.by_id.insert(
            task_id.to_string(),
            CachedRow {
                stamp,
                envelope,
                row,
            },
        );
        if is_new {
            entries.order.push_back(task_id.to_string());
            while entries.by_id.len() > MAX_CACHED_TASK_ROWS {
                let Some(oldest) = entries.order.pop_front() else {
                    break;
                };
                entries.by_id.remove(&oldest);
            }
        }
    }

    pub(super) fn forget(&self, task_id: &str) {
        self.entries().by_id.remove(task_id);
    }

    /// A poisoned lock only means some thread panicked while holding it.
    /// Every entry is re-proved against the filesystem before it is reused,
    /// so the cache keeps serving rather than failing an unrelated read.
    fn entries(&self) -> MutexGuard<'_, RowCacheEntries> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
