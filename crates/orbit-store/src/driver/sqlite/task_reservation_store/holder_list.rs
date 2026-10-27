//! The holder lists a hard lease request or expansion answers with and writes
//! to its `queued` notice (`docs/design/path-leases`, token resource bounds).
//!
//! Whether a request or selector is blocked is decided from every holder, but
//! a lease-token answer shows only the holders that fit its byte budget. A
//! [`HolderList`] keeps, while the holders are scanned, only the conflicts
//! that sort first and could still fit, so it never holds a full
//! selector-by-holder list; `queued_ahead` then takes what remains of the
//! budget, in queue order.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::contracts::{TaskLockConflict, TaskLockHolder};

/// A conflict in answer order: requested selector, then task locks,
/// reservations and hard leases, then holder id.
type ConflictKey = (String, u8, String);

/// The bounded `conflicts` and `queued_ahead` of one hard request or
/// expansion. Unbounded (`None` bytes) for SSH and dashboard callers.
pub(super) struct HolderList {
    remaining: Option<usize>,
    /// The most conflicts `remaining` could show, each costing at least the
    /// smallest serialized conflict.
    conflict_limit: usize,
    conflicts: BTreeSet<ConflictKey>,
    truncated: bool,
}

impl HolderList {
    /// A list whose entries may serialize to at most `bytes`, each entry
    /// costing its JSON plus one separating byte.
    pub(super) fn new(bytes: Option<usize>) -> Self {
        Self {
            remaining: bytes,
            conflict_limit: bytes.map_or(usize::MAX, |bytes| bytes / smallest_conflict_bytes()),
            conflicts: BTreeSet::new(),
            truncated: false,
        }
    }

    /// Keep `conflict` if it is among the first conflicts the budget could
    /// show; duplicates are kept once.
    pub(super) fn offer(&mut self, conflict: TaskLockConflict) {
        self.conflicts.insert((
            conflict.file,
            holder_rank(conflict.held_by),
            conflict.held_by_id,
        ));
        if self.conflicts.len() > self.conflict_limit {
            self.conflicts.pop_last();
            self.truncated = true;
        }
    }

    /// The kept conflicts that fit the budget, in answer order. Spends their
    /// bytes; call it once, before `admit`ting `queued_ahead`.
    pub(super) fn take_conflicts(&mut self) -> Vec<TaskLockConflict> {
        let mut shown = Vec::new();
        for (file, rank, held_by_id) in std::mem::take(&mut self.conflicts) {
            let conflict = TaskLockConflict {
                file,
                held_by: holder_for_rank(rank),
                held_by_id,
            };
            if !self.admit(&conflict) {
                break;
            }
            shown.push(conflict);
        }
        shown
    }

    /// Whether `entry` still fits; once one does not, nothing more does, so
    /// each list stays a prefix.
    pub(super) fn admit(&mut self, entry: &impl Serialize) -> bool {
        let Some(remaining) = self.remaining.as_mut() else {
            return true;
        };
        let size = serialized_len(entry) + 1;
        if self.truncated || size > *remaining {
            self.truncated = true;
            return false;
        }
        *remaining -= size;
        true
    }

    /// Whether any holder was left out.
    pub(super) fn truncated(&self) -> bool {
        self.truncated
    }
}

fn smallest_conflict_bytes() -> usize {
    serialized_len(&TaskLockConflict {
        file: String::new(),
        held_by: TaskLockHolder::Task,
        held_by_id: String::new(),
    }) + 1
}

fn serialized_len(value: &impl Serialize) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX / 2, |bytes| bytes.len())
}

fn holder_rank(holder: TaskLockHolder) -> u8 {
    match holder {
        TaskLockHolder::Task => 0,
        TaskLockHolder::Reservation => 1,
        TaskLockHolder::Lease => 2,
    }
}

fn holder_for_rank(rank: u8) -> TaskLockHolder {
    match rank {
        0 => TaskLockHolder::Task,
        1 => TaskLockHolder::Reservation,
        _ => TaskLockHolder::Lease,
    }
}
