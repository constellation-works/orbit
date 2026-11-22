//! Carried ledger and open-task footprint reconciliation for `comment-hygiene`.

use std::collections::BTreeMap;

use orbit_common::fs::overlap_index::OverlapIndex;
use orbit_types::task::{TaskStatus, TaskType};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskListFilter;

use super::{COMMENT_HYGIENE_ESCALATION_TAG, COMMENT_HYGIENE_TAG, HoldReason, is_iso_date};

/// Artifact path carrying the versioned comment-hygiene attempt ledger.
const LEDGER_ARTIFACT_PATH: &str = "comment-hygiene-ledger.json";
/// Predecessor read window specified by `comment-hygiene.yaml`.
const LEDGER_WINDOW: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AttemptOutcome {
    Clean,
    Fixed,
    Partial,
    Skipped,
}

impl AttemptOutcome {
    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "clean" => Some(Self::Clean),
            "fixed" => Some(Self::Fixed),
            "partial" => Some(Self::Partial),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }

    pub(super) fn is_clean_or_fixed(self) -> bool {
        matches!(self, Self::Clean | Self::Fixed)
    }

    pub(super) fn is_skip_or_partial(self) -> bool {
        matches!(self, Self::Partial | Self::Skipped)
    }
}

#[derive(Debug, Clone)]
pub(super) struct AttemptRow {
    pub(super) attempted_on: String,
    pub(super) outcome: AttemptOutcome,
    pub(super) reason: Option<String>,
    pub(super) checked_through: Option<String>,
    /// Ordering tie-breaker: higher value means a newer predecessor task.
    recency_rank: usize,
}

#[derive(Debug, Default)]
pub(super) struct LedgerResolution {
    pub(super) status: &'static str,
    pub(super) completed_tasks_scanned: usize,
    pub(super) base_v2_task_id: Option<String>,
    pub(super) v1_layered_task_ids: Vec<String>,
    pub(super) carried_state: BTreeMap<String, Vec<AttemptRow>>,
    pub(super) dropped_untracked_paths: usize,
    pub(super) notes: String,
}

#[derive(Debug, Default)]
pub(super) struct OpenTaskScan {
    pub(super) status: &'static str,
    pub(super) tasks_scanned: usize,
    pub(super) selectors_indexed: usize,
    pub(super) index: OverlapIndex<String>,
}

pub(super) fn ledger_json(ledger: &LedgerResolution) -> Value {
    json!({
        "status": ledger.status,
        "completed_tasks_scanned": ledger.completed_tasks_scanned,
        "base_v2_task_id": ledger.base_v2_task_id,
        "v1_layered_task_ids": ledger.v1_layered_task_ids,
        "carried_paths": ledger.carried_state.len(),
        "dropped_untracked_paths": ledger.dropped_untracked_paths,
        "notes": ledger.notes,
    })
}

pub(super) fn open_tasks_json(open_tasks: &OpenTaskScan) -> Value {
    json!({
        "status": open_tasks.status,
        "tasks_scanned": open_tasks.tasks_scanned,
        "selectors_indexed": open_tasks.selectors_indexed,
    })
}

pub(super) fn load_carried_ledger(
    runtime: &OrbitRuntime,
    hold_reasons: &mut Vec<HoldReason>,
) -> LedgerResolution {
    let mut resolution = LedgerResolution {
        status: "missing_or_unreadable",
        ..Default::default()
    };
    let filter = TaskListFilter {
        statuses: Some(vec![TaskStatus::Done]),
        task_type: Some(TaskType::Chore),
        tags: vec![COMMENT_HYGIENE_TAG.to_string()],
        ..Default::default()
    };
    let candidates = match runtime.task_candidates(&filter, usize::MAX) {
        Ok(candidates) => candidates,
        Err(error) => {
            hold_reasons.push(HoldReason {
                code: "ledger_tasks_unavailable",
                source: "orbit.task.list".to_string(),
                detail: format!("could not list completed comment-hygiene tasks: {error}"),
            });
            resolution.notes = "completed comment-hygiene tasks could not be listed".to_string();
            return resolution;
        }
    };
    if candidates.total > candidates.items.len() {
        hold_reasons.push(HoldReason {
            code: "ledger_tasks_truncated",
            source: "orbit.task.list".to_string(),
            detail: format!(
                "completed comment-hygiene task listing truncated ({} of {})",
                candidates.items.len(),
                candidates.total
            ),
        });
        resolution.notes = "completed comment-hygiene task listing was truncated".to_string();
        return resolution;
    }

    let mut predecessors = candidates
        .items
        .into_iter()
        .filter(|task| {
            !task
                .tags
                .iter()
                .any(|tag| tag == COMMENT_HYGIENE_ESCALATION_TAG)
        })
        .collect::<Vec<_>>();
    // Newest first, with deterministic ID tie-break so later-created tasks win
    // even if two tasks share a coarse timestamp.
    predecessors.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| right.id.cmp(&left.id))
    });

    if predecessors.is_empty() {
        resolution.status = "no_prior_instances";
        resolution.notes =
            "no completed comment-hygiene instances in workspace; ordering by git commit date"
                .to_string();
        return resolution;
    }

    let mut base_v2_state: Option<BTreeMap<String, Vec<AttemptRow>>> = None;
    let mut v1_layers: Vec<(String, Vec<(String, AttemptRow)>)> = Vec::new();
    let window = predecessors.len().min(LEDGER_WINDOW);

    for (index, pred) in predecessors.iter().take(window).enumerate() {
        resolution.completed_tasks_scanned += 1;
        let recency_rank = window.saturating_sub(index);
        let manifest = match runtime.get_task_artifact_manifest(&pred.id) {
            Ok(manifest) => manifest,
            Err(error) => {
                hold_reasons.push(HoldReason {
                    code: "ledger_manifest_unreadable",
                    source: pred.id.clone(),
                    detail: format!(
                        "could not read artifact manifest for predecessor {}: {error}",
                        pred.id
                    ),
                });
                continue;
            }
        };
        let has_ledger = manifest
            .iter()
            .any(|entry| entry.path == LEDGER_ARTIFACT_PATH);
        if !has_ledger {
            hold_reasons.push(HoldReason {
                code: "missing_predecessor_ledger",
                source: format!("{}:{LEDGER_ARTIFACT_PATH}", pred.id),
                detail: format!(
                    "completed comment-hygiene predecessor {} is missing required artifact {LEDGER_ARTIFACT_PATH}",
                    pred.id
                ),
            });
            continue;
        }
        let artifact = match runtime.get_task_artifact(&pred.id, LEDGER_ARTIFACT_PATH) {
            Ok(Some(artifact)) => artifact,
            Ok(None) => {
                hold_reasons.push(HoldReason {
                    code: "ledger_missing_blob",
                    source: format!("{}:{LEDGER_ARTIFACT_PATH}", pred.id),
                    detail: format!(
                        "manifest for predecessor {} lists {LEDGER_ARTIFACT_PATH} but the artifact content is missing",
                        pred.id
                    ),
                });
                continue;
            }
            Err(error) => {
                hold_reasons.push(HoldReason {
                    code: "ledger_unreadable",
                    source: format!("{}:{LEDGER_ARTIFACT_PATH}", pred.id),
                    detail: format!(
                        "could not read {LEDGER_ARTIFACT_PATH} for predecessor {}: {error}",
                        pred.id
                    ),
                });
                continue;
            }
        };
        let parsed: Value = match serde_json::from_slice(&artifact.content) {
            Ok(value) => value,
            Err(error) => {
                hold_reasons.push(HoldReason {
                    code: "ledger_malformed",
                    source: format!("{}:{LEDGER_ARTIFACT_PATH}", pred.id),
                    detail: format!(
                        "{LEDGER_ARTIFACT_PATH} on predecessor {} is invalid JSON: {error}",
                        pred.id
                    ),
                });
                continue;
            }
        };
        match parsed.get("schema_version").and_then(Value::as_u64) {
            Some(2) => match parse_v2_ledger_state(&parsed, recency_rank) {
                Ok(state) => {
                    resolution.base_v2_task_id = Some(pred.id.clone());
                    base_v2_state = Some(state);
                    break;
                }
                Err(detail) => {
                    hold_reasons.push(HoldReason {
                        code: "ledger_malformed",
                        source: format!("{}:{LEDGER_ARTIFACT_PATH}", pred.id),
                        detail: format!(
                            "{LEDGER_ARTIFACT_PATH} on predecessor {} has malformed schema_version 2 state: {detail}",
                            pred.id
                        ),
                    });
                }
            },
            Some(1) => match parse_v1_ledger_files(&parsed, recency_rank) {
                Ok(rows) => {
                    resolution.v1_layered_task_ids.push(pred.id.clone());
                    v1_layers.push((pred.id.clone(), rows));
                }
                Err(detail) => {
                    hold_reasons.push(HoldReason {
                        code: "ledger_malformed",
                        source: format!("{}:{LEDGER_ARTIFACT_PATH}", pred.id),
                        detail: format!(
                            "{LEDGER_ARTIFACT_PATH} on predecessor {} has malformed schema_version 1 files: {detail}",
                            pred.id
                        ),
                    });
                }
            },
            other => {
                hold_reasons.push(HoldReason {
                    code: "ledger_unsupported_schema",
                    source: format!("{}:{LEDGER_ARTIFACT_PATH}", pred.id),
                    detail: format!(
                        "{LEDGER_ARTIFACT_PATH} on predecessor {} has unsupported schema_version {other:?}",
                        pred.id
                    ),
                });
            }
        }
    }

    if !hold_reasons.is_empty() {
        resolution.notes = "required predecessor ledger evidence missing or unreadable".to_string();
        return resolution;
    }

    if base_v2_state.is_none() && predecessors.len() > LEDGER_WINDOW {
        hold_reasons.push(HoldReason {
            code: "ledger_history_truncated",
            source: LEDGER_ARTIFACT_PATH.to_string(),
            detail: format!(
                "found {} completed comment-hygiene predecessors with no cumulative schema_version 2 ledger in the newest {LEDGER_WINDOW}",
                predecessors.len()
            ),
        });
        resolution.notes =
            "ledger history truncated past 60-task window without a v2 ledger".into();
        return resolution;
    }

    let mut state = base_v2_state.unwrap_or_default();
    // Layer v1 ledgers oldest -> newest so newer tasks win ties on attempted_on.
    for (_, rows) in v1_layers.into_iter().rev() {
        for (path, row) in rows {
            state.entry(path).or_default().push(row);
        }
    }
    for rows in state.values_mut() {
        rows.sort_by(|left, right| {
            right
                .attempted_on
                .cmp(&left.attempted_on)
                .then_with(|| right.recency_rank.cmp(&left.recency_rank))
        });
        rows.truncate(2);
    }

    resolution.status = "loaded";
    resolution.carried_state = state;
    resolution.notes = match &resolution.base_v2_task_id {
        Some(base_id) if resolution.v1_layered_task_ids.is_empty() => {
            format!("loaded cumulative schema_version 2 ledger from {base_id}")
        }
        Some(base_id) => format!(
            "loaded cumulative schema_version 2 ledger from {base_id} with {} newer schema_version 1 ledger(s) layered on top",
            resolution.v1_layered_task_ids.len()
        ),
        None => format!(
            "loaded {} schema_version 1 ledger(s) across all completed predecessors",
            resolution.v1_layered_task_ids.len()
        ),
    };
    resolution
}

fn parse_v2_ledger_state(
    ledger: &Value,
    recency_rank: usize,
) -> Result<BTreeMap<String, Vec<AttemptRow>>, String> {
    let state = ledger
        .get("state")
        .and_then(Value::as_object)
        .ok_or_else(|| "missing `state` object".to_string())?;
    let mut parsed = BTreeMap::new();
    for (path, entries) in state {
        let rows = entries
            .as_array()
            .ok_or_else(|| format!("state[{path:?}] must be an array"))?;
        let mut parsed_rows = Vec::with_capacity(rows.len());
        for (idx, row) in rows.iter().enumerate() {
            parsed_rows.push(parse_attempt_row(
                row,
                recency_rank,
                &format!("state[{path:?}][{idx}]"),
            )?);
        }
        parsed.insert(path.clone(), parsed_rows);
    }
    Ok(parsed)
}

fn parse_v1_ledger_files(
    ledger: &Value,
    recency_rank: usize,
) -> Result<Vec<(String, AttemptRow)>, String> {
    let files = ledger
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing `files` array".to_string())?;
    let mut parsed = Vec::with_capacity(files.len());
    for (idx, row) in files.iter().enumerate() {
        let path = row
            .get("path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("files[{idx}] is missing non-empty `path`"))?
            .to_string();
        let attempt = parse_attempt_row(row, recency_rank, &format!("files[{idx}]"))?;
        parsed.push((path, attempt));
    }
    Ok(parsed)
}

fn parse_attempt_row(row: &Value, recency_rank: usize, label: &str) -> Result<AttemptRow, String> {
    let object = row
        .as_object()
        .ok_or_else(|| format!("{label} must be an object"))?;
    let attempted_on = object
        .get("attempted_on")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| is_iso_date(value))
        .ok_or_else(|| format!("{label} must have a YYYY-MM-DD `attempted_on` date"))?
        .to_string();
    let outcome_raw = object
        .get("outcome")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{label} is missing `outcome`"))?;
    let outcome = AttemptOutcome::parse(outcome_raw)
        .ok_or_else(|| format!("{label} has invalid outcome {outcome_raw:?}"))?;
    let reason = object
        .get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let checked_through = object
        .get("checked_through")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    Ok(AttemptRow {
        attempted_on,
        outcome,
        reason,
        checked_through,
        recency_rank,
    })
}

pub(super) fn load_open_task_footprints(
    runtime: &OrbitRuntime,
    current_task_id: &str,
    hold_reasons: &mut Vec<HoldReason>,
) -> OpenTaskScan {
    let mut scan = OpenTaskScan {
        status: "unavailable",
        ..Default::default()
    };
    let filter = TaskListFilter {
        statuses: Some(vec![
            TaskStatus::Proposed,
            TaskStatus::Backlog,
            TaskStatus::InProgress,
            TaskStatus::Review,
            TaskStatus::Blocked,
        ]),
        ..Default::default()
    };
    let candidates = match runtime.task_candidates(&filter, usize::MAX) {
        Ok(candidates) => candidates,
        Err(error) => {
            hold_reasons.push(HoldReason {
                code: "open_task_footprints_unavailable",
                source: "orbit.task.list".to_string(),
                detail: format!("could not list open task footprints: {error}"),
            });
            return scan;
        }
    };
    if candidates.total > candidates.items.len() {
        hold_reasons.push(HoldReason {
            code: "open_task_footprints_truncated",
            source: "orbit.task.list".to_string(),
            detail: format!(
                "open task footprint listing truncated ({} of {})",
                candidates.items.len(),
                candidates.total
            ),
        });
        scan.status = "incomplete";
        return scan;
    }

    let mut unparseable = false;
    for open_task in &candidates.items {
        if open_task.id == current_task_id {
            continue;
        }
        scan.tasks_scanned += 1;
        for selector in &open_task.context_files {
            if scan.index.insert(selector, open_task.id.clone()) {
                scan.selectors_indexed += 1;
            } else {
                unparseable = true;
                hold_reasons.push(HoldReason {
                    code: "open_task_selector_unparseable",
                    source: format!("{}:{selector}", open_task.id),
                    detail: format!(
                        "open task {} has unparseable context_files selector {selector:?}; cannot verify footprint overlap",
                        open_task.id
                    ),
                });
            }
        }
    }
    scan.status = if unparseable {
        "incomplete"
    } else {
        "complete"
    };
    scan
}
