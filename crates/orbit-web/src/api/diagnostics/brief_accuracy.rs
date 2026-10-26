//! Explore-brief file predictions against landed diffs, and actual dispatch
//! outcomes. Missing evidence is counted separately, never scored as zero.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use orbit_common::fs::selector::Selector;
use orbit_core::{OrbitError, OrbitRuntime, V2AuditEventFilter};
use orbit_types::task::{EXPLORE_BRIEF_ARTIFACT, TaskStatus};
use orbit_types::workflow::{ExploreBriefOutcome, V2AuditEvent, V2AuditEventKind};
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::{DiagnosticsQuery, map_runtime_error};
use super::metrics::DiagnosticsRange;
use crate::state::Ws;

const AUDIT_LIMIT: usize = 50_000;

#[derive(Default, Deserialize)]
pub(in crate::api) struct BriefAccuracyQuery {
    since: Option<String>,
    month: Option<String>,
    /// Match any current task tag, for example the trial's brief arm.
    tags: Option<String>,
}

pub(in crate::api) async fn diagnostics_brief_accuracy(
    Ws(runtime): Ws,
    Query(q): Query<BriefAccuracyQuery>,
) -> Response {
    let tags: BTreeSet<String> = q
        .tags
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect();
    if tags.len() > 8 {
        return map_runtime_error(OrbitError::InvalidInput(
            "tags accepts at most eight distinct tags".to_string(),
        ));
    }
    let range = match DiagnosticsRange::from_query(
        &DiagnosticsQuery {
            outcome: None,
            since: q.since,
            month: q.month,
            limit: None,
        },
        false,
    ) {
        Ok(range) => range,
        Err(error) => return map_runtime_error(error),
    };
    match super::super::blocking("brief accuracy", move || {
        brief_accuracy_report(&runtime, &range, &tags)
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

#[derive(Deserialize)]
struct BriefDocument {
    schema: String,
    task_id: String,
    brief: BriefTargets,
}

#[derive(Deserialize)]
struct BriefTargets {
    change: Vec<ChangeTarget>,
}

#[derive(Deserialize)]
struct ChangeTarget {
    target: String,
}

/// The metric is about file predictions. Symbol anchors name a file;
/// directories make no finite file prediction and are disclosed separately.
fn brief_file_targets(content: &[u8], task_id: &str) -> Option<(BTreeSet<String>, usize)> {
    let document: BriefDocument = serde_json::from_slice(content).ok()?;
    if document.schema != "explore_brief.v1"
        || document.task_id != task_id
        || document.brief.change.is_empty()
        || document.brief.change.len() > 12
    {
        return None;
    }
    let mut files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    for change in document.brief.change {
        match Selector::from_str(&change.target).ok()? {
            Selector::File { path } | Selector::Symbol { path, .. } => {
                files.insert(path);
            }
            Selector::Dir { path } => {
                directories.insert(path);
            }
            _ => return None,
        }
    }
    Some((files, directories.len()))
}

fn brief_accuracy_report(
    runtime: &OrbitRuntime,
    range: &DiagnosticsRange,
    tags: &BTreeSet<String>,
) -> Result<Value, OrbitError> {
    // Use the same terminal index and date convention as completions.
    let tasks: BTreeSet<String> = runtime
        .task_terminal_transitions(range.since)?
        .into_iter()
        .filter(|row| {
            row.status == TaskStatus::Done
                && range.contains(row.at)
                && (tags.is_empty() || row.tags.iter().any(|tag| tags.contains(tag)))
        })
        .map(|row| row.task_id)
        .collect();

    let mut predicted = BTreeMap::new();
    let mut no_brief = 0;
    let mut invalid_brief = 0;
    let mut directory_targets = 0;
    let mut no_file_targets = 0;
    for id in &tasks {
        let Some(artifact) = runtime.get_task_artifact(id, EXPLORE_BRIEF_ARTIFACT)? else {
            no_brief += 1;
            continue;
        };
        let Some((files, directories)) = brief_file_targets(&artifact.content, id) else {
            invalid_brief += 1;
            continue;
        };
        directory_targets += directories;
        if files.is_empty() {
            no_file_targets += 1;
            continue;
        }
        predicted.insert(id.clone(), files);
    }

    // A task can have several deliveries. Union its paths, but do not score
    // a partial diff if any of its recorded ranges is unavailable locally.
    let deliveries: Vec<_> = runtime
        .landed_diff_stats(None)?
        .into_iter()
        .filter(|row| predicted.contains_key(&row.task_id) && row.landed_at <= range.until)
        .collect();
    let mut actual: BTreeMap<&str, Option<BTreeSet<String>>> = BTreeMap::new();
    for (row, paths) in deliveries
        .iter()
        .zip(runtime.landed_diff_file_paths(&deliveries))
    {
        let entry = actual
            .entry(&row.task_id)
            .or_insert_with(|| Some(BTreeSet::new()));
        match (entry.as_mut(), paths) {
            (Some(files), Some(paths)) => files.extend(paths),
            _ => *entry = None,
        }
    }
    let mut scored = 0;
    let mut no_diff = 0;
    let mut unavailable_diff = 0;
    let mut predicted_files = 0;
    let mut delivered_files = 0;
    let mut matched_files = 0;
    for (id, files) in &predicted {
        match actual.get(id.as_str()) {
            None => no_diff += 1,
            Some(None) => unavailable_diff += 1,
            Some(Some(paths)) => {
                scored += 1;
                predicted_files += files.len();
                delivered_files += paths.len();
                matched_files += files.intersection(paths).count();
            }
        }
    }

    // Dispatch can predate completion by weeks. Query the actual event kind
    // across history, then join to this report's done tasks. Do not infer an
    // absent outcome from a missing artifact or a missing audit row.
    let mut rows = runtime.list_v2_audit_events(V2AuditEventFilter {
        source: Some("v2_envelope".to_string()),
        body_kinds: vec!["explore_brief_applied".to_string()],
        until: Some(range.until),
        limit: Some(AUDIT_LIMIT + 1),
        ..Default::default()
    })?;
    let truncated = rows.len() > AUDIT_LIMIT;
    rows.truncate(AUDIT_LIMIT);
    let mut outcomes =
        BTreeMap::from([("fresh", 0), ("drifted", 0), ("dropped", 0), ("absent", 0)]);
    let mut seen = BTreeSet::new();
    let mut observed_tasks = BTreeSet::new();
    let mut invalid_events = 0;
    for row in rows {
        let Ok(event) = serde_json::from_str::<V2AuditEvent>(&row.payload_json) else {
            invalid_events += 1;
            continue;
        };
        let V2AuditEventKind::ExploreBriefApplied { task_id, brief, .. } = event.kind else {
            continue;
        };
        if !tasks.contains(&task_id) || !seen.insert(event.envelope.event_id) {
            continue;
        }
        observed_tasks.insert(task_id);
        let key = match brief.outcome {
            ExploreBriefOutcome::Fresh => "fresh",
            ExploreBriefOutcome::Drifted => "drifted",
            ExploreBriefOutcome::Dropped => "dropped",
            ExploreBriefOutcome::Absent => "absent",
        };
        *outcomes.entry(key).or_default() += 1;
    }
    let ratio =
        |denominator: usize| (denominator > 0).then(|| matched_files as f64 / denominator as f64);
    Ok(json!({
        "schema_version": 1,
        "since": range.since,
        "until": range.until,
        "tags": tags,
        "done_tasks": tasks.len(),
        "accuracy": {
            "scored_tasks": scored,
            "predicted_files": predicted_files,
            "delivered_files": delivered_files,
            "matched_files": matched_files,
            "recall": ratio(delivered_files),
            "precision": ratio(predicted_files),
            "excluded_no_brief": no_brief,
            "excluded_invalid_brief": invalid_brief,
            "excluded_no_file_targets": no_file_targets,
            "excluded_no_diff": no_diff,
            "excluded_unavailable_diff": unavailable_diff,
            "directory_targets": directory_targets,
        },
        "outcomes": outcomes,
        "dispatches": seen.len(),
        "tasks_without_outcome": tasks.len() - observed_tasks.len(),
        "invalid_audit_events": invalid_events,
        "truncated": truncated,
    }))
}
