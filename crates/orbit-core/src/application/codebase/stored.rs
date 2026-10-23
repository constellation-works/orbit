//! Per-task [`HistoryFacts`] kept in the store between scans, so a scan reads
//! only the bundles of tasks that changed since the last one.
//!
//! A row is reused only while it was written in [`FACTS_FORMAT_VERSION`] and
//! its source still matches: the task's history stamp (its event and comment
//! logs' length, modification time and file identity, taken without reading
//! them) plus a digest of the task's failed claim settlements, which classify
//! a `claim_failed` block that carries no note. A new history event, a
//! comment, a reopen or a new settlement therefore recomputes that task, and
//! a format change recomputes every row instead of misreading it. A task the
//! store cannot stamp (a write to settle, a worker reading through its
//! owner, a platform without file identity) is computed at every scan and
//! never stored.
//!
//! Inputs that change without the task are not part of a row: envelope
//! fields come from the task index, pilot runs are attributed and lock
//! denials and reservations are read at every scan, and a landed commit
//! whose objects are missing is diffed again until they appear.
//!
//! The rows are a derived cache. Unreadable or unwritable rows only cost the
//! reads they would have saved, and dropping the table rebuilds it on the
//! next scan with identical facts.

use std::collections::{BTreeSet, HashMap};

use orbit_common::security::release::sha256_hex;
use orbit_store::contracts::{CodebaseFactsStoreBackend, CodebaseTaskFactsRow};

use super::facts::{HistoryFacts, SettledFailure};
use crate::OrbitRuntime;

/// Bump when [`HistoryFacts`] or anything it serializes changes shape or
/// meaning: every stored row is then recomputed and replaced.
pub(super) const FACTS_FORMAT_VERSION: u32 = 2;

/// One task's facts during a scan, and whether they need writing back.
pub(super) struct Entry {
    pub(super) facts: HistoryFacts,
    /// What the facts were computed from; `None` when they must not be
    /// stored.
    source: Option<String>,
    /// Computed or completed in this scan, so the stored row is out of date.
    pub(super) dirty: bool,
}

/// The workspace's stored rows, read once per scan, and the rows the scan
/// will write back.
pub(super) struct StoredFacts {
    /// `None` when the rows could not be read: nothing is reused or written.
    workspace_id: Option<String>,
    rows: HashMap<String, CodebaseTaskFactsRow>,
    writes: Vec<CodebaseTaskFactsRow>,
}

impl StoredFacts {
    pub(super) fn load(runtime: &OrbitRuntime) -> Self {
        let loaded = runtime.workspace_id().and_then(|workspace_id| {
            let rows = runtime.sqlite_store()?.codebase_task_facts(&workspace_id)?;
            Ok((workspace_id, rows))
        });
        let (workspace_id, rows) = match loaded {
            Ok((workspace_id, rows)) => (
                Some(workspace_id),
                rows.into_iter()
                    .map(|row| (row.task_id.clone(), row))
                    .collect(),
            ),
            Err(error) => {
                tracing::warn!(%error, "codebase analytics: stored task facts unreadable; scanning every task");
                (None, HashMap::new())
            }
        };
        Self {
            workspace_id,
            rows,
            writes: Vec::new(),
        }
    }

    /// The task's stored facts while they are current, else freshly computed.
    pub(super) fn entry(
        &self,
        runtime: &OrbitRuntime,
        id: &str,
        settlements: &[SettledFailure],
    ) -> Entry {
        // The stamp is taken before any read it labels.
        let source = self
            .workspace_id
            .as_ref()
            .and_then(|_| runtime.task_history_stamp(id))
            .map(|stamp| format!("{stamp}|{}", settlement_digest(settlements)));
        if let Some(source) = &source
            && let Some(row) = self.rows.get(id)
            && row.format_version == FACTS_FORMAT_VERSION
            && row.source == *source
            && let Ok(facts) = serde_json::from_str(&row.facts_json)
        {
            return Entry {
                facts,
                source: Some(source.clone()),
                dirty: false,
            };
        }
        let (facts, complete) = HistoryFacts::compute(runtime, id, settlements);
        Entry {
            facts,
            source: source.filter(|_| complete),
            dirty: true,
        }
    }

    /// Queue `entry` for writing when it changed and may be stored.
    pub(super) fn keep(&mut self, id: &str, entry: &Entry) {
        let Some(source) = entry.source.as_ref().filter(|_| entry.dirty) else {
            return;
        };
        match serde_json::to_string(&entry.facts) {
            Ok(facts_json) => self.writes.push(CodebaseTaskFactsRow {
                task_id: id.to_string(),
                format_version: FACTS_FORMAT_VERSION,
                source: source.clone(),
                facts_json,
            }),
            Err(error) => {
                tracing::warn!(task_id = %id, %error, "codebase analytics: task facts not stored");
            }
        }
    }

    /// Write the queued rows and drop the rows of tasks the scan no longer
    /// listed. Best effort: a read-only or busy store keeps its old rows.
    pub(super) fn save<'a>(self, runtime: &OrbitRuntime, listed: impl Iterator<Item = &'a str>) {
        let Some(workspace_id) = self.workspace_id else {
            return;
        };
        let listed: BTreeSet<&str> = listed.collect();
        let removed: Vec<String> = self
            .rows
            .into_keys()
            .filter(|id| !listed.contains(id.as_str()))
            .collect();
        if self.writes.is_empty() && removed.is_empty() {
            return;
        }
        let written = runtime.sqlite_store().and_then(|store| {
            if store.is_read_only() {
                return Ok(());
            }
            store.write_codebase_task_facts(&workspace_id, &self.writes, &removed)
        });
        if let Err(error) = written {
            tracing::warn!(%error, "codebase analytics: task facts not stored; the next scan recomputes them");
        }
    }
}

/// The task's failed claim settlements, as one value that changes with any
/// of them.
fn settlement_digest(settlements: &[SettledFailure]) -> String {
    if settlements.is_empty() {
        return String::new();
    }
    sha256_hex(format!("{settlements:?}").as_bytes())
}
