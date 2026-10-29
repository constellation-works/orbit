//! The first-edit series behind `turns_before_first_edit` and
//! `result_bytes_before_first_edit`, read once for both the experiment
//! readout and Analytics › Metrics so they share one definition.
//!
//! A task scores from its first implement invocation: the earlier of
//! `implement_one` and `agent_implement` by `(ts, id)`. Turns are the tool
//! calls strictly before the first file-mutating call, after ordering by
//! `seq`; result bytes are the saturating sum of those preceding calls. An
//! edit at the first call scores zero. An invocation with no recorded tool
//! call, or none whose name is a known editor tool, has no score. A shell
//! such as `command_execution` is not an edit, and is never averaged as zero.
//! An empty trace is separately observable from a populated trace with no
//! detectable edit.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::InvocationQuery;

use crate::RuntimeKernel;
use crate::cost::IMPLEMENT_INVOCATION_IDS;

/// Rows fetched per page. A limit of 0 is the store's silent cap of 100, so
/// every page asks for an explicit size.
const PAGE_LIMIT: usize = 10_000;
/// Invocations kept per implement activity. Reaching it truncates the read.
const MAX_RECORDS: usize = 100_000;
/// Editor tool names, matched case-insensitively against the full name and
/// against the tail after the last `__` (an MCP tool's own name). Shell
/// tools are absent on purpose: a provider that edits only through a shell
/// has no detectable first edit.
const FILE_MUTATING: &[&str] = &[
    "edit",
    "write",
    "multiedit",
    "multi_edit",
    "notebookedit",
    "editnotebook",
    "notebook_edit",
    "apply_patch",
    "applypatch",
    "str_replace",
    "strreplace",
    "search_replace",
    "edit_file",
    "write_file",
    "create_file",
    "delete_file",
    "replace",
];

/// The first implement invocation linked to one task.
#[derive(Debug, Clone, PartialEq)]
pub struct FirstImplementEdit {
    pub ts: DateTime<Utc>,
    pub id: i64,
    pub agent: String,
    /// `(turns, result bytes)` before the first edit, or `None` when the
    /// invocation records no detectable edit.
    pub score: Option<(u64, u64)>,
    /// Whether the invocation recorded no tool calls.
    pub empty_trace: bool,
}

/// The first implement invocation of each task, from one read.
#[derive(Debug, Default)]
pub struct FirstImplementEdits {
    pub by_task: HashMap<String, FirstImplementEdit>,
    /// An implement activity reached [`MAX_RECORDS`], so an older task's
    /// first invocation may be missing from `by_task`.
    pub truncated: bool,
}

impl RuntimeKernel {
    /// The first implement invocation of each task linked to this
    /// workspace's implement invocations recorded at or after `since` (all of
    /// them for `None`). A task whose first invocation the read does not
    /// hold has no entry.
    pub fn first_implement_edits(
        &self,
        since: Option<DateTime<Utc>>,
    ) -> Result<FirstImplementEdits, OrbitError> {
        let mut edits = FirstImplementEdits::default();
        for activity in IMPLEMENT_INVOCATION_IDS {
            let truncated = self.fold_activity_first_edits(activity, since, &mut edits.by_task)?;
            edits.truncated |= truncated;
        }
        Ok(edits)
    }

    /// Pages one activity newest-first and keeps, per task, the earliest
    /// invocation by `(ts, id)`. Returns whether the record cap cut the walk
    /// short.
    fn fold_activity_first_edits(
        &self,
        activity: &str,
        since: Option<DateTime<Utc>>,
        by_task: &mut HashMap<String, FirstImplementEdit>,
    ) -> Result<bool, OrbitError> {
        let mut seen = HashSet::new();
        let mut kept = 0usize;
        let mut until: Option<DateTime<Utc>> = None;
        loop {
            if kept >= MAX_RECORDS {
                return Ok(true);
            }
            let page = self.workspace_invocation_records(InvocationQuery {
                since,
                until,
                activity_id: Some(activity.to_string()),
                limit: PAGE_LIMIT,
                ..Default::default()
            })?;
            let Some(oldest) = page.last().map(|record| record.ts) else {
                return Ok(false);
            };
            let page_len = page.len();
            let mut added = 0usize;
            for record in page {
                if !seen.insert(record.id) {
                    continue;
                }
                if kept >= MAX_RECORDS {
                    return Ok(true);
                }
                kept += 1;
                added += 1;
                let score = turns_before_edit(
                    record
                        .tool_calls
                        .iter()
                        .map(|call| (call.seq, call.tool_name.as_str(), call.result_bytes)),
                );
                let empty_trace = record.tool_calls.is_empty();
                for task_id in record.task_ids {
                    let replace = by_task
                        .get(&task_id)
                        .is_none_or(|existing| (record.ts, record.id) < (existing.ts, existing.id));
                    if replace {
                        by_task.insert(
                            task_id,
                            FirstImplementEdit {
                                ts: record.ts,
                                id: record.id,
                                agent: record.agent.clone(),
                                score,
                                empty_trace,
                            },
                        );
                    }
                }
            }
            if page_len < PAGE_LIMIT {
                return Ok(false);
            }
            // A full page that adds nothing new means the store keeps
            // returning rows we have seen: older rows may remain unread.
            if added == 0 {
                return Ok(true);
            }
            until = Some(oldest);
        }
    }
}

/// True when `name` is a known file-editing tool, including the tool's own
/// name on the tail of an MCP identifier.
fn is_file_mutating(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let tail = match lower.rsplit_once("__") {
        Some((_, tail)) => tail,
        None => lower.as_str(),
    };
    FILE_MUTATING
        .iter()
        .any(|known| *known == tail || *known == lower.as_str())
}

/// Turns and result bytes before the first edit, or `None` when no call is a
/// detectable edit. `None` is an exclusion, including an empty trace. Each
/// item is `(seq, tool_name, result_bytes)`.
fn turns_before_edit<'a>(
    calls: impl IntoIterator<Item = (u64, &'a str, u64)>,
) -> Option<(u64, u64)> {
    let mut ordered: Vec<(u64, &str, u64)> = calls.into_iter().collect();
    if ordered.is_empty() {
        return None;
    }
    ordered.sort_by_key(|call| call.0);
    let index = ordered.iter().position(|call| is_file_mutating(call.1))?;
    let bytes = ordered[..index]
        .iter()
        .fold(0u64, |sum, call| sum.saturating_add(call.2));
    Some((u64::try_from(index).unwrap_or(u64::MAX), bytes))
}
