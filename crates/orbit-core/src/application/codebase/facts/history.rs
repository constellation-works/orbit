//! Per-task history reduction kept between workspace scans.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, RegressionRiskMarks, TaskDecision,
    TaskStatus,
};
use serde::{Deserialize, Serialize};

use super::block_cause::{SettledFailure, classify_block, entry_marks_run};
use super::scan::entered;
use super::types::{Anchor, Block};
use crate::OrbitRuntime;

/// What one task's history, comments and failed claim settlements say,
/// apart from its envelope: everything a scan otherwise rereads the bundle
/// for, and what [`super::super::stored`] keeps between scans. Envelope fields
/// (status, title, footprint, tags, run) are joined at every scan, so an
/// envelope edit never needs these recomputed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(in crate::application::codebase) struct HistoryFacts {
    /// When the task last entered `done`, whatever its status now.
    pub(in crate::application::codebase) entered_done: Option<DateTime<Utc>>,
    /// When the task last entered `rejected`, whatever its status now.
    pub(in crate::application::codebase) entered_rejected: Option<DateTime<Utc>>,
    pub(in crate::application::codebase) widenings: Vec<(DateTime<Utc>, Vec<Anchor>)>,
    /// Whether a history entry only a job run writes is present.
    pub(in crate::application::codebase) marks_run: bool,
    pub(in crate::application::codebase) blocks: Vec<Block>,
    /// Ascending.
    pub(in crate::application::codebase) left_backlog: Vec<DateTime<Utc>>,
    /// The verified landing receipt: its commit and when it landed.
    pub(in crate::application::codebase) receipt: Option<(String, DateTime<Utc>)>,
    pub(in crate::application::codebase) marks: Vec<(DateTime<Utc>, RegressionRiskMarks)>,
    /// The task-pilot receipts, read for a task with a landing receipt.
    pub(in crate::application::codebase) pilot: Option<super::super::pilot::PilotInputs>,
    /// The receipt commit's changed paths, once a diff produced them. Never
    /// stored as unavailable: missing objects may be fetched later.
    pub(in crate::application::codebase) landed_paths: Option<BTreeSet<String>>,
}

impl HistoryFacts {
    /// Read the task's history (and, for a landed task, its comments) and
    /// derive its facts. The flag is false when a read failed: the facts
    /// then stand for an empty history and must not be stored.
    pub(in crate::application::codebase) fn compute(
        runtime: &OrbitRuntime,
        id: &str,
        settlements: &[SettledFailure],
    ) -> (Self, bool) {
        let _span = tracing::trace_span!(
            target: "orbit.core.codebase",
            "codebase_task_facts_computed",
            task_id = %id,
        )
        .entered();
        let mut complete = true;
        let history = runtime.get_task_history(id).unwrap_or_else(|error| {
            tracing::warn!(task_id = %id, %error, "codebase analytics: task history unreadable");
            complete = false;
            Vec::new()
        });
        let widenings = history
            .iter()
            .filter(|entry| entry.event == CONTEXT_FILES_WIDENED_EVENT)
            .filter_map(|entry| {
                let widening = ContextFilesWidening::from_entry(entry)?;
                let anchors = widening
                    .selectors
                    .iter()
                    .filter_map(|selector| Anchor::parse(selector))
                    .collect::<Vec<_>>();
                Some((entry.at, anchors))
            })
            .collect();
        let blocks = history
            .iter()
            .filter(|entry| entry.to_status == Some(TaskStatus::Blocked))
            .map(|entry| Block {
                at: entry.at,
                event: entry.event.clone(),
                cause: classify_block(entry, settlements),
            })
            .collect();
        let mut left_backlog: Vec<DateTime<Utc>> = history
            .iter()
            .filter(|entry| entry.to_status.is_some_and(|to| to != TaskStatus::Backlog))
            .map(|entry| entry.at)
            .collect();
        left_backlog.sort();
        let marks = history
            .iter()
            .filter_map(|entry| match &entry.decision {
                Some(TaskDecision::RegressionRiskMarked(marked)) => {
                    Some((entry.at, marked.clone()))
                }
                _ => None,
            })
            .collect();
        let receipt = super::super::landed::receipt(&history);
        let pilot = receipt
            .is_some()
            .then(|| {
                super::super::pilot::inputs(runtime, id).unwrap_or_else(|error| {
                    tracing::warn!(task_id = %id, %error, "codebase analytics: task comments unreadable");
                    complete = false;
                    None
                })
            })
            .flatten();
        let facts = Self {
            entered_done: entered(&history, TaskStatus::Done),
            entered_rejected: entered(&history, TaskStatus::Rejected),
            widenings,
            marks_run: history.iter().any(entry_marks_run),
            blocks,
            left_backlog,
            receipt,
            marks,
            pilot,
            landed_paths: None,
        };
        (facts, complete)
    }
}
