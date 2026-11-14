//! Per-task [`TaskFacts`] kept in the store between scans, so a scan reads
//! only the history and comments of tasks that changed since the last one or
//! are still open.
//!
//! A row is reused only for a terminal task with no open attempt span while it
//! was written in [`FACTS_FORMAT_VERSION`] and its source still matches: the
//! task's history stamp (its event and comment logs' length, modification time
//! and file identity, taken without reading them) plus a digest of the task's
//! other per-task inputs (its envelope fields, failed claim settlements,
//! delivery runs and pulled claims). A new history event, a comment, an
//! envelope edit, a new run or a claim settlement therefore recomputes that
//! task, and a format change recomputes every row instead of misreading it.
//! Non-terminal tasks, tasks with an open span, and tasks the store cannot
//! stamp or whose history or comments read failed are computed at every scan
//! and never stored.
//!
//! Cross-task steps (`judge_rejects`, pilot assessment run attribution, landed
//! diff lines, `mark_implementer_less_attempts` and attribution) run on every
//! scan after the stored row is queued, so per-scan mutations never reach the
//! stored row.
//!
//! The rows are a derived cache. Unreadable or unwritable rows only cost the
//! reads they would have saved, and dropping the table rebuilds it on the
//! next scan with identical facts.

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use orbit_common::security::release::sha256_hex;
use orbit_store::contracts::{AgentFactsStoreBackend, AgentTaskFactsRow};
use orbit_types::task::{Task, TaskHistoryEntry};

use super::super::records::block_cause;
use super::model::{RunRecord, Runner, TaskFacts};
use super::task::task_facts;
use crate::OrbitRuntime;
use crate::application::codebase::SettledFailure;

/// Bump when [`TaskFacts`] or anything it serializes changes shape or
/// meaning: every stored row is then recomputed and replaced. Version 1
/// wrote a pulled attempt's `Some(None)` provider-default report as JSON
/// `null` and read it back as no report [ORB-24640]; version 2 keeps the
/// three states, and version-1 rows are recomputed from their settlement
/// and history inputs instead of being reused.
pub(super) const FACTS_FORMAT_VERSION: u32 = 2;

/// One task's facts during a scan, and whether they need writing back.
pub(super) struct Entry {
    pub(super) facts: TaskFacts,
    /// What the facts were computed from; `None` when they must not be
    /// stored.
    source: Option<String>,
    /// Computed in this scan, so the stored row is out of date.
    pub(super) dirty: bool,
}

/// The workspace's stored rows, read once per scan, and the rows the scan
/// will write back.
pub(super) struct StoredFacts {
    /// `None` when the rows could not be read: nothing is reused or written.
    workspace_id: Option<String>,
    pulled_digest: String,
    rows: HashMap<String, AgentTaskFactsRow>,
    writes: Vec<AgentTaskFactsRow>,
}

impl StoredFacts {
    pub(super) fn load(runtime: &OrbitRuntime, pulled: &HashMap<&str, Runner>) -> Self {
        let loaded = runtime.workspace_id().and_then(|workspace_id| {
            let rows = runtime.sqlite_store()?.agent_task_facts(&workspace_id)?;
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
                tracing::warn!(%error, "agent scorecards: stored task facts unreadable; scanning every task");
                (None, HashMap::new())
            }
        };
        Self {
            workspace_id,
            pulled_digest: pulled_digest(pulled),
            rows,
            writes: Vec::new(),
        }
    }

    /// The task's stored facts while they are current, else freshly computed.
    pub(super) fn entry(
        &self,
        runtime: &OrbitRuntime,
        task: &Task,
        settlements: &[SettledFailure],
        runs: &[RunRecord],
        pulled: &HashMap<&str, Runner>,
        computed_at: DateTime<Utc>,
    ) -> Entry {
        let id = task.id.as_str();
        // Only terminal tasks can have no open span; the stamp is taken before
        // any read it labels.
        let source = self
            .workspace_id
            .as_ref()
            .filter(|_| task.status.is_terminal())
            .and_then(|_| runtime.task_history_stamp(id))
            .map(|stamp| {
                format!(
                    "{stamp}|{}",
                    inputs_digest(task, settlements, runs, &self.pulled_digest)
                )
            });
        if let Some(source) = &source
            && let Some(row) = self.rows.get(id)
            && row.format_version == FACTS_FORMAT_VERSION
            && row.source == *source
            && let Ok(facts) = serde_json::from_str::<TaskFacts>(&row.facts_json)
            && has_no_open_span(&facts)
        {
            return Entry {
                facts,
                source: Some(source.clone()),
                dirty: false,
            };
        }
        let mut complete = true;
        let history = runtime.get_task_history(id).unwrap_or_else(|error| {
            complete = false;
            tracing::warn!(task_id = %id, %error, "agent scorecards: task history unreadable");
            Vec::new()
        });
        let comments = runtime.get_task_pilot_comments(id).unwrap_or_else(|error| {
            complete = false;
            tracing::warn!(task_id = %id, %error, "agent scorecards: task comments unreadable");
            Vec::new()
        });
        let classify = |entry: &TaskHistoryEntry| block_cause(entry, &history, settlements);
        let facts = task_facts(
            task,
            &history,
            &comments,
            runs,
            pulled,
            &classify,
            computed_at,
        );
        let storeable = complete && has_no_open_span(&facts);
        Entry {
            facts,
            source: source.filter(|_| storeable),
            dirty: true,
        }
    }

    /// Queue `entry` for writing when it changed and may be stored.
    pub(super) fn keep(&mut self, id: &str, entry: &Entry) {
        let Some(source) = entry.source.as_ref().filter(|_| entry.dirty) else {
            return;
        };
        match serde_json::to_string(&entry.facts) {
            Ok(facts_json) => self.writes.push(AgentTaskFactsRow {
                task_id: id.to_string(),
                format_version: FACTS_FORMAT_VERSION,
                source: source.clone(),
                facts_json,
            }),
            Err(error) => {
                tracing::warn!(task_id = %id, %error, "agent scorecards: task facts not stored");
            }
        }
    }

    /// Write the queued rows and drop the rows of tasks the scan no longer
    /// retained. Best effort: a read-only or busy store keeps its old rows.
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
            store.write_agent_task_facts(&workspace_id, &self.writes, &removed)
        });
        if let Err(error) = written {
            tracing::warn!(%error, "agent scorecards: task facts not stored; the next scan recomputes them");
        }
    }
}

fn has_no_open_span(facts: &TaskFacts) -> bool {
    facts.attempts.iter().all(|attempt| attempt.end.is_some())
}

fn pulled_digest(pulled: &HashMap<&str, Runner>) -> String {
    if pulled.is_empty() {
        return String::new();
    }
    let mut entries: Vec<(&str, &Runner)> = pulled.iter().map(|(id, r)| (*id, r)).collect();
    entries.sort_by_key(|(id, _)| *id);
    sha256_hex(format!("{entries:?}").as_bytes())
}

/// The non-history inputs to [`task_facts`], as one value that changes with
/// any of them.
fn inputs_digest(
    task: &Task,
    settlements: &[SettledFailure],
    runs: &[RunRecord],
    pulled_digest: &str,
) -> String {
    let envelope = (
        task.updated_at,
        &task.title,
        task.status,
        &task.crew,
        &task.task_type,
        &task.tags,
        &task.context_files,
        task.complexity,
        task.created_at,
        &task.relations,
        &task.external_refs,
    );
    let inputs = (envelope, settlements, runs, pulled_digest);
    sha256_hex(format!("{inputs:?}").as_bytes())
}
