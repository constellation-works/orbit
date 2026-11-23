//! Task classification, candidate ranking and the deterministic batch
//! preparation pass, including the evidence-less hold payload.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::fs::lock_mode::FootprintLockModes;
use orbit_types::task::{Task, TaskType};
use serde_json::{Value, json};

use crate::OrbitRuntime;

use super::super::source::SourceSnapshot;

use super::exclusions::{
    GitAttributeRules, PathExclusion, classify_path_exclusion, count_lines,
    has_generated_header_marker, is_binary_bytes,
};
use super::git::{
    commit_exists, latest_commit_dates, list_tracked_blobs, paths_touched_between, read_blob_batch,
    read_single_blob,
};
use super::ledger::{
    LedgerResolution, OpenTaskScan, ledger_json, load_carried_ledger, load_open_task_footprints,
    open_tasks_json,
};

use super::{
    BLOB_BATCH_SIZE, COMMENT_HYGIENE_ESCALATION_TAG, COMMENT_HYGIENE_TAG, HoldReason, LINE_BUDGET,
    MAX_BATCH_FILES, MAX_RANKED_CANDIDATES, MAX_REPORTED_EXCLUSIONS, OVERSIZED_LINE_THRESHOLD,
};

/// Whether `task` is a selector-free `comment-hygiene` chore awaiting
/// deterministic batch preparation.
pub(in crate::application::task::pilot) fn is_comment_hygiene_batch_task(task: &Task) -> bool {
    task.task_type == TaskType::Chore
        && task.context_files.is_empty()
        && task.tags.iter().any(|tag| tag == COMMENT_HYGIENE_TAG)
        && !task
            .tags
            .iter()
            .any(|tag| tag == COMMENT_HYGIENE_ESCALATION_TAG)
}

/// Whether a prepared `comment_hygiene_batch` payload holds promotion.
pub(in crate::application::task::pilot) fn comment_hygiene_batch_holds(batch: &Value) -> bool {
    batch.get("status").and_then(Value::as_str) == Some("hold")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum CandidateBucket {
    ChangedSinceLastCheck,
    NeverChecked,
}

impl CandidateBucket {
    fn as_str(self) -> &'static str {
        match self {
            Self::ChangedSinceLastCheck => "changed_since_last_check",
            Self::NeverChecked => "never_checked",
        }
    }
}

#[derive(Debug, Clone)]
struct PreScoredCandidate {
    path: String,
    oid: String,
    bucket: CandidateBucket,
    ordering_date: String,
    ordering_source: &'static str,
    checked_through: Option<String>,
    checked_through_unresolved: bool,
}

#[derive(Debug, Clone)]
struct RankedCandidate {
    selector: String,
    path: String,
    lines: usize,
    bucket: CandidateBucket,
    ordering_date: String,
    ordering_source: &'static str,
    checked_through: Option<String>,
    checked_through_unresolved: bool,
    ranged: bool,
}

impl RankedCandidate {
    fn to_json(&self) -> Value {
        json!({
            "selector": self.selector,
            "path": self.path,
            "lines": self.lines,
            "bucket": self.bucket.as_str(),
            "ordering_date": self.ordering_date,
            "ordering_source": self.ordering_source,
            "checked_through": self.checked_through,
            "checked_through_unresolved": self.checked_through_unresolved,
            "ranged": self.ranged,
        })
    }
}

#[derive(Debug, Clone)]
struct OpenTaskExclusion {
    path: String,
    task_id: String,
    selector: String,
}

#[derive(Debug, Clone)]
struct RepeatSkipHold {
    path: String,
    reasons: Vec<String>,
}

/// Deterministically prepare the oldest-first `comment-hygiene` batch for
/// `task` at the pinned `source` revision.
pub(in crate::application::task::pilot) fn prepare_comment_hygiene_batch(
    runtime: &OrbitRuntime,
    workspace_root: &Path,
    task: &Task,
    source: Option<&SourceSnapshot>,
) -> Value {
    let mut hold_reasons = Vec::new();
    let source_revision = source.map(|snapshot| snapshot.source_revision.as_str());

    let Some(revision) = source_revision else {
        hold_reasons.push(HoldReason {
            code: "missing_pinned_source",
            source: workspace_root.display().to_string(),
            detail: "comment-hygiene batch preparation requires a pinned Git source revision"
                .to_string(),
        });
        return hold_payload(None, &hold_reasons, None, None, 0, &BTreeMap::new());
    };

    // 1. Tracked blobs at pinned source_revision.
    let mut excluded_by_rule: BTreeMap<String, usize> = BTreeMap::new();
    let tracked_blobs = match list_tracked_blobs(workspace_root, revision, &mut excluded_by_rule) {
        Ok(blobs) => blobs,
        Err(detail) => {
            hold_reasons.push(HoldReason {
                code: "git_inventory_unavailable",
                source: revision.to_string(),
                detail,
            });
            return hold_payload(
                Some(revision),
                &hold_reasons,
                None,
                None,
                0,
                &excluded_by_rule,
            );
        }
    };
    let tracked_count = tracked_blobs.len();

    // 2. Carried ledger state from completed comment-hygiene predecessors.
    let mut ledger = load_carried_ledger(runtime, &mut hold_reasons);

    // 3. Open-task footprints (checked even if ledger held so every missing
    //    evidence source is reported together).
    let open_tasks = load_open_task_footprints(runtime, &task.id, &mut hold_reasons);

    if !hold_reasons.is_empty() {
        return hold_payload(
            Some(revision),
            &hold_reasons,
            Some(&ledger),
            Some(&open_tasks),
            tracked_count,
            &excluded_by_rule,
        );
    }

    // Drop carried-state paths no longer tracked at source_revision.
    let before_drop = ledger.carried_state.len();
    ledger
        .carried_state
        .retain(|path, _| tracked_blobs.contains_key(path));
    ledger.dropped_untracked_paths = before_drop.saturating_sub(ledger.carried_state.len());

    // Read .gitattributes at source_revision if tracked.
    let git_attrs = match tracked_blobs.get(".gitattributes") {
        Some(oid) => match read_single_blob(workspace_root, oid) {
            Ok(bytes) => GitAttributeRules::parse(&String::from_utf8_lossy(&bytes)),
            Err(detail) => {
                hold_reasons.push(HoldReason {
                    code: "git_blob_unavailable",
                    source: ".gitattributes".to_string(),
                    detail,
                });
                return hold_payload(
                    Some(revision),
                    &hold_reasons,
                    Some(&ledger),
                    Some(&open_tasks),
                    tracked_count,
                    &excluded_by_rule,
                );
            }
        },
        None => GitAttributeRules::default(),
    };

    // Apply path-based and .gitattributes exclusions.
    let mut eligible_blobs: BTreeMap<String, String> = BTreeMap::new();
    for (path, oid) in &tracked_blobs {
        if let Some(reason) = classify_path_exclusion(path, &git_attrs) {
            *excluded_by_rule
                .entry(reason.key().to_string())
                .or_insert(0) += 1;
            continue;
        }
        eligible_blobs.insert(path.clone(), oid.clone());
    }

    // Drop files unchanged since their last clean/fixed check.
    let mut check_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut unresolved_checked_through: BTreeSet<String> = BTreeSet::new();
    for (path, rows) in &ledger.carried_state {
        if !eligible_blobs.contains_key(path) {
            continue;
        }
        let Some(latest) = rows.first() else {
            continue;
        };
        if !latest.outcome.is_clean_or_fixed() {
            continue;
        }
        match latest
            .checked_through
            .as_deref()
            .map(str::trim)
            .filter(|sha| !sha.is_empty())
        {
            Some(sha) => check_groups
                .entry(sha.to_string())
                .or_default()
                .push(path.clone()),
            None => {
                unresolved_checked_through.insert(path.clone());
            }
        }
    }

    let mut unchanged_paths: BTreeSet<String> = BTreeSet::new();
    for (checked_through, paths) in &check_groups {
        if checked_through == revision {
            unchanged_paths.extend(paths.iter().cloned());
            continue;
        }
        match commit_exists(workspace_root, checked_through) {
            Ok(false) => {
                unresolved_checked_through.extend(paths.iter().cloned());
            }
            Ok(true) => match paths_touched_between(workspace_root, checked_through, revision) {
                Ok(touched) => {
                    for path in paths {
                        if !touched.contains(path) {
                            unchanged_paths.insert(path.clone());
                        }
                    }
                }
                Err(detail) => {
                    hold_reasons.push(HoldReason {
                        code: "git_history_unavailable",
                        source: format!("{checked_through}..{revision}"),
                        detail,
                    });
                    return hold_payload(
                        Some(revision),
                        &hold_reasons,
                        Some(&ledger),
                        Some(&open_tasks),
                        tracked_count,
                        &excluded_by_rule,
                    );
                }
            },
            Err(detail) => {
                hold_reasons.push(HoldReason {
                    code: "git_history_unavailable",
                    source: checked_through.clone(),
                    detail,
                });
                return hold_payload(
                    Some(revision),
                    &hold_reasons,
                    Some(&ledger),
                    Some(&open_tasks),
                    tracked_count,
                    &excluded_by_rule,
                );
            }
        }
    }

    // Exclude open-task footprints and hold repeat skips.
    let modes = FootprintLockModes::default();
    let mut open_task_excluded = Vec::new();
    let mut repeat_skip_held = Vec::new();
    let mut changed_candidates = Vec::new();
    let mut never_checked_paths: BTreeMap<String, String> = BTreeMap::new();

    for (path, oid) in &eligible_blobs {
        if unchanged_paths.contains(path) {
            continue;
        }
        let file_selector = format!("file:{path}");
        let overlaps = open_tasks.index.overlapping(&file_selector, &modes);
        if let Some((holding_selector, holding_task_id)) = overlaps.first() {
            open_task_excluded.push(OpenTaskExclusion {
                path: path.clone(),
                task_id: (*holding_task_id).clone(),
                selector: (*holding_selector).to_string(),
            });
            continue;
        }
        if let Some(rows) = ledger.carried_state.get(path)
            && !rows.is_empty()
        {
            if rows.len() >= 2
                && rows[0].outcome.is_skip_or_partial()
                && rows[1].outcome.is_skip_or_partial()
            {
                repeat_skip_held.push(RepeatSkipHold {
                    path: path.clone(),
                    reasons: vec![
                        rows[0]
                            .reason
                            .clone()
                            .unwrap_or_else(|| "no reason recorded".to_string()),
                        rows[1]
                            .reason
                            .clone()
                            .unwrap_or_else(|| "no reason recorded".to_string()),
                    ],
                });
                continue;
            }
            let latest = &rows[0];
            changed_candidates.push(PreScoredCandidate {
                path: path.clone(),
                oid: oid.clone(),
                bucket: CandidateBucket::ChangedSinceLastCheck,
                ordering_date: latest.attempted_on.clone(),
                ordering_source: "ledger",
                checked_through: latest.checked_through.clone(),
                checked_through_unresolved: unresolved_checked_through.contains(path),
            });
        } else {
            never_checked_paths.insert(path.clone(), oid.clone());
        }
    }

    // Resolve git commit dates for never-checked candidates.
    let git_dates = if never_checked_paths.is_empty() {
        BTreeMap::new()
    } else {
        let needed: BTreeSet<String> = never_checked_paths.keys().cloned().collect();
        match latest_commit_dates(workspace_root, revision, &needed) {
            Ok(dates) => dates,
            Err(detail) => {
                hold_reasons.push(HoldReason {
                    code: "git_history_unavailable",
                    source: revision.to_string(),
                    detail,
                });
                return hold_payload(
                    Some(revision),
                    &hold_reasons,
                    Some(&ledger),
                    Some(&open_tasks),
                    tracked_count,
                    &excluded_by_rule,
                );
            }
        }
    };

    let mut all_candidates = changed_candidates;
    for (path, oid) in never_checked_paths {
        let Some(date) = git_dates.get(&path) else {
            hold_reasons.push(HoldReason {
                code: "missing_git_history",
                source: path.clone(),
                detail: format!(
                    "tracked source file `{path}` has no resolvable commit date at {revision}"
                ),
            });
            continue;
        };
        all_candidates.push(PreScoredCandidate {
            path,
            oid,
            bucket: CandidateBucket::NeverChecked,
            ordering_date: date.clone(),
            ordering_source: "git",
            checked_through: None,
            checked_through_unresolved: false,
        });
    }

    if !hold_reasons.is_empty() {
        return hold_payload(
            Some(revision),
            &hold_reasons,
            Some(&ledger),
            Some(&open_tasks),
            tracked_count,
            &excluded_by_rule,
        );
    }

    all_candidates.sort_by(|left, right| {
        left.bucket
            .cmp(&right.bucket)
            .then_with(|| left.ordering_date.cmp(&right.ordering_date))
            .then_with(|| left.path.cmp(&right.path))
    });

    // Inspect candidate blobs in sorted order for binary / generated markers
    // and exact line counts, filling `selected` and bounded `ranked_candidates`.
    let mut ranked_candidates: Vec<RankedCandidate> = Vec::new();
    let mut selected: Vec<RankedCandidate> = Vec::new();
    let mut total_selected_lines: usize = 0;
    let mut batch_closed = false;
    let mut content_excluded = 0usize;

    let mut cursor = 0usize;
    while cursor < all_candidates.len() && ranked_candidates.len() < MAX_RANKED_CANDIDATES {
        let end = (cursor + BLOB_BATCH_SIZE).min(all_candidates.len());
        let slice = &all_candidates[cursor..end];
        let oids: Vec<&str> = slice.iter().map(|item| item.oid.as_str()).collect();
        let blobs = match read_blob_batch(workspace_root, &oids) {
            Ok(blobs) => blobs,
            Err(detail) => {
                hold_reasons.push(HoldReason {
                    code: "git_blob_unavailable",
                    source: revision.to_string(),
                    detail,
                });
                return hold_payload(
                    Some(revision),
                    &hold_reasons,
                    Some(&ledger),
                    Some(&open_tasks),
                    tracked_count,
                    &excluded_by_rule,
                );
            }
        };
        for (candidate, bytes) in slice.iter().zip(blobs) {
            if is_binary_bytes(&bytes) {
                *excluded_by_rule
                    .entry(PathExclusion::ArchiveOrBinary.key().to_string())
                    .or_insert(0) += 1;
                content_excluded += 1;
                continue;
            }
            if has_generated_header_marker(&bytes) {
                *excluded_by_rule
                    .entry("generated_marker".to_string())
                    .or_insert(0) += 1;
                content_excluded += 1;
                continue;
            }
            let lines = count_lines(&bytes);
            let oversized = lines > OVERSIZED_LINE_THRESHOLD;
            let entry = RankedCandidate {
                selector: format!("file:{}", candidate.path),
                path: candidate.path.clone(),
                lines,
                bucket: candidate.bucket,
                ordering_date: candidate.ordering_date.clone(),
                ordering_source: candidate.ordering_source,
                checked_through: candidate.checked_through.clone(),
                checked_through_unresolved: candidate.checked_through_unresolved,
                ranged: oversized,
            };
            if !batch_closed {
                if selected.is_empty() {
                    total_selected_lines = total_selected_lines.saturating_add(lines);
                    selected.push(entry.clone());
                    if oversized
                        || total_selected_lines >= LINE_BUDGET
                        || selected.len() >= MAX_BATCH_FILES
                    {
                        batch_closed = true;
                    }
                } else if oversized
                    || selected.len() >= MAX_BATCH_FILES
                    || total_selected_lines.saturating_add(lines) > LINE_BUDGET
                {
                    batch_closed = true;
                } else {
                    total_selected_lines = total_selected_lines.saturating_add(lines);
                    selected.push(entry.clone());
                    if selected.len() >= MAX_BATCH_FILES || total_selected_lines >= LINE_BUDGET {
                        batch_closed = true;
                    }
                }
            }
            ranked_candidates.push(entry);
            if ranked_candidates.len() >= MAX_RANKED_CANDIDATES {
                break;
            }
        }
        cursor = end;
    }

    let status = if selected.is_empty() {
        "empty"
    } else {
        "ready"
    };
    let selectors: Vec<String> = selected.iter().map(|item| item.selector.clone()).collect();
    let eligible_remaining = all_candidates.len().saturating_sub(content_excluded);

    json!({
        "schema_version": 1,
        "status": status,
        "source_revision": revision,
        "hold_reasons": [],
        "selectors": selectors,
        "selected": selected.iter().map(RankedCandidate::to_json).collect::<Vec<_>>(),
        "ranked_candidates": ranked_candidates.iter().map(RankedCandidate::to_json).collect::<Vec<_>>(),
        "total_selected_lines": total_selected_lines,
        "line_budget": LINE_BUDGET,
        "max_files": MAX_BATCH_FILES,
        "oversized_threshold": OVERSIZED_LINE_THRESHOLD,
        "inventory": {
            "tracked_files": tracked_count,
            "excluded_by_rule": excluded_by_rule,
            "unchanged_dropped": unchanged_paths.len(),
            "open_task_excluded_count": open_task_excluded.len(),
            "open_task_excluded": open_task_excluded
                .iter()
                .take(MAX_REPORTED_EXCLUSIONS)
                .map(|row| json!({
                    "path": row.path,
                    "task_id": row.task_id,
                    "selector": row.selector,
                }))
                .collect::<Vec<_>>(),
            "repeat_skip_held_count": repeat_skip_held.len(),
            "repeat_skip_held": repeat_skip_held
                .iter()
                .take(MAX_REPORTED_EXCLUSIONS)
                .map(|row| json!({
                    "path": row.path,
                    "reasons": row.reasons,
                }))
                .collect::<Vec<_>>(),
            "eligible_candidates": eligible_remaining,
        },
        "ledger": ledger_json(&ledger),
        "open_tasks": open_tasks_json(&open_tasks),
    })
}

fn hold_payload(
    source_revision: Option<&str>,
    hold_reasons: &[HoldReason],
    ledger: Option<&LedgerResolution>,
    open_tasks: Option<&OpenTaskScan>,
    tracked_files: usize,
    excluded_by_rule: &BTreeMap<String, usize>,
) -> Value {
    json!({
        "schema_version": 1,
        "status": "hold",
        "source_revision": source_revision,
        "hold_reasons": hold_reasons.iter().map(HoldReason::to_json).collect::<Vec<_>>(),
        "selectors": [],
        "selected": [],
        "ranked_candidates": [],
        "total_selected_lines": 0,
        "line_budget": LINE_BUDGET,
        "max_files": MAX_BATCH_FILES,
        "oversized_threshold": OVERSIZED_LINE_THRESHOLD,
        "inventory": {
            "tracked_files": tracked_files,
            "excluded_by_rule": excluded_by_rule,
            "unchanged_dropped": 0,
            "open_task_excluded_count": 0,
            "open_task_excluded": [],
            "repeat_skip_held_count": 0,
            "repeat_skip_held": [],
            "eligible_candidates": 0,
        },
        "ledger": ledger.map(ledger_json).unwrap_or_else(|| json!({
            "status": "missing_or_unreadable",
            "completed_tasks_scanned": 0,
            "base_v2_task_id": Value::Null,
            "v1_layered_task_ids": [],
            "skipped_without_ledger_task_ids": [],
            "carried_paths": 0,
            "dropped_untracked_paths": 0,
            "notes": "ledger reconciliation did not complete",
        })),
        "open_tasks": open_tasks.map(open_tasks_json).unwrap_or_else(|| json!({
            "status": "unavailable",
            "tasks_scanned": 0,
            "selectors_indexed": 0,
        })),
    })
}
