use orbit_types::task::{TaskComplexity, TaskStatus};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

use super::run::{MAX_EXCLUDED_SAMPLE, NO_DIFF_TAGS};

/// Bounded evidence for routine (non-explicit) discovery exclusions: every
/// excluded task is counted by reason, but only a capped sample is itemized
/// so response size stops scaling with terminal workspace history.
#[derive(Default)]
pub(super) struct ExcludedEvidence {
    pub(super) sample: Vec<Value>,
    pub(super) total: usize,
    pub(super) by_reason: BTreeMap<&'static str, usize>,
}

impl ExcludedEvidence {
    pub(super) fn record(
        &mut self,
        task_id: &str,
        reason: &'static str,
        active_preparations: &BTreeMap<String, BTreeSet<String>>,
    ) {
        self.total += 1;
        *self.by_reason.entry(reason).or_default() += 1;
        // Explicit selections report every held task; only automatic discovery
        // samples exclusions from potentially unbounded workspace history.
        if reason != "already_preparing" && self.sample.len() >= MAX_EXCLUDED_SAMPLE {
            return;
        }
        let mut entry = json!({ "task_id": task_id, "reason": reason });
        if matches!(reason, "active_pilot_prepared" | "already_preparing")
            && let Some(run_ids) = active_preparations.get(task_id)
            && let Value::Object(fields) = &mut entry
        {
            fields.insert("prepared_by_run_ids".to_string(), json!(run_ids));
        }
        self.sample.push(entry);
    }
}

pub(super) fn automatic_exclusion_reason(
    status: TaskStatus,
    context_files: &[String],
    complexity: Option<TaskComplexity>,
    tags: &[String],
) -> Option<&'static str> {
    if !matches!(status, TaskStatus::Proposed | TaskStatus::Backlog) {
        return Some("status_not_eligible");
    }
    if !context_files.is_empty() && complexity.is_some_and(TaskComplexity::is_assessed) {
        return Some("context_files_not_empty");
    }
    // No-diff work produces its durable result outside the repository, so
    // there are no modification selectors to assess. Automatically minted
    // no-diff tasks are no exception: admission already exempts
    // `no-diff-expected` from the assessed-complexity gate [ORB-12118], and a
    // `verified_no_diff` assessment leaves `context_files` empty, so admitting
    // them here re-piloted the same task on every routine tick.
    no_diff_exclusion_reason(tags)
}

/// The no-diff exclusion alone: an independent gate that the assessed
/// footprint reason above can hide, checked separately wherever that
/// reason is waived for a stale repilot.
pub(super) fn no_diff_exclusion_reason(tags: &[String]) -> Option<&'static str> {
    tags.iter()
        .any(|tag| NO_DIFF_TAGS.contains(&tag.as_str()))
        .then_some("no_diff_task")
}

pub(super) fn task_snapshot(
    id: &str,
    title: &str,
    status: TaskStatus,
    complexity: Option<TaskComplexity>,
    tags: &[String],
    context_files: &[String],
) -> Value {
    json!({
        "task_id": id,
        "title": title,
        "status": status,
        "complexity": complexity,
        "tags": tags,
        "context_files_before": context_files,
    })
}
