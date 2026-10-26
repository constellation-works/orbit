//! Tool calls and result bytes before the first file edit, by complexity
//! and tag cohort, for Analytics › Metrics.
//!
//! Each terminal task counts once, in the bucket of its index `updated_at`.
//! The score comes from that task's first implement invocation: the earlier
//! of `implement_one` and `agent_implement` by `(ts, id)`. Turns are the
//! tool calls strictly before the first file-mutating call, after ordering
//! by `seq`; result bytes are the saturating sum of those preceding calls.
//! An edit at the first call scores zero. An invocation with no recorded
//! tool call, or none whose name is a known editor tool, is excluded and
//! counted — a shell such as `command_execution` is not an edit, and is
//! never averaged as zero.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_core::metrics::cost::IMPLEMENT_INVOCATION_IDS;
use orbit_core::{InvocationQuery, OrbitRuntime};
use orbit_types::task::{TaskStatus, UNSET_BUCKET, complexity_bucket_ord};
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::{DiagnosticsQuery, map_runtime_error};
use super::metrics::DiagnosticsRange;
use super::trend::{TrendWindow, trend_range};
use crate::state::Ws;

/// Complexities a request may name; `unassessed` is an alias of the unset
/// bucket.
const COMPLEXITIES: [&str; 5] = ["low", "medium", "hard", "xhard", UNSET_BUCKET];
/// Series drawn even when the window holds none of their tasks.
const STANDING_SERIES: [&str; 4] = ["low", "medium", "hard", "xhard"];
/// A point averaging fewer tasks than this is marked thin.
const MIN_TASKS: usize = 3;
/// Tags one request may split on.
const MAX_TAGS: usize = 8;
/// Tags listed for the picker, most used first.
const AVAILABLE_TAGS: usize = 200;
/// How far before the oldest task's filing the invocation read starts.
const READ_SLACK: chrono::Duration = chrono::Duration::days(1);
/// Rows fetched per page. A limit of 0 is the store's silent cap of 100,
/// so every page asks for an explicit size.
const PAGE_LIMIT: usize = 10_000;
/// Invocations kept per implement activity. Reaching it sets `truncated`.
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

#[derive(Deserialize, Default)]
pub(in crate::api) struct TurnsToEditQuery {
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    month: Option<String>,
    /// `done` (default) or `all` terminal statuses.
    #[serde(default)]
    outcome: Option<String>,
    /// Comma-separated complexities to draw; every one when absent.
    #[serde(default)]
    complexity: Option<String>,
    /// Comma-separated tags; a task carrying any of them is "with tag".
    #[serde(default)]
    tags: Option<String>,
    /// Only tasks whose first implement invocation ran as this agent.
    #[serde(default)]
    provider: Option<String>,
}

struct Filters {
    done_only: bool,
    /// `None` draws every complexity present.
    complexities: Option<BTreeSet<&'static str>>,
    tags: Vec<String>,
    provider: Option<String>,
}

fn comma_list(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_filters(q: &TurnsToEditQuery) -> Result<Filters, orbit_core::OrbitError> {
    let invalid = |message: String| orbit_core::OrbitError::InvalidInput(message);
    let done_only = match q.outcome.as_deref().unwrap_or("done") {
        "done" => true,
        "all" => false,
        other => {
            return Err(invalid(format!(
                "unknown outcome '{other}': expected done or all"
            )));
        }
    };
    let requested = comma_list(q.complexity.as_deref());
    let complexities = if requested.is_empty() {
        None
    } else {
        let mut set = BTreeSet::new();
        for value in &requested {
            let value = if value == "unassessed" {
                UNSET_BUCKET
            } else {
                value
            };
            let Some(known) = COMPLEXITIES.iter().find(|known| **known == value) else {
                return Err(invalid(format!(
                    "unknown complexity '{value}': expected low, medium, hard, xhard or unassessed"
                )));
            };
            set.insert(*known);
        }
        Some(set)
    };
    let mut tags = comma_list(q.tags.as_deref());
    tags.sort();
    tags.dedup();
    if tags.len() > MAX_TAGS {
        return Err(invalid(format!(
            "too many tags: at most {MAX_TAGS} may be compared"
        )));
    }
    let provider = q
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|provider| !provider.is_empty())
        .map(str::to_string);
    Ok(Filters {
        done_only,
        complexities,
        tags,
        provider,
    })
}

/// `GET /api/diagnostics/turns_to_edit` — turns and result bytes before the
/// first file edit of each task's first implement invocation.
pub(in crate::api) async fn diagnostics_turns_to_edit(
    Ws(runtime): Ws,
    Query(q): Query<TurnsToEditQuery>,
) -> Response {
    let filters = match parse_filters(&q) {
        Ok(filters) => filters,
        Err(error) => return map_runtime_error(error),
    };
    let range = match trend_range(&DiagnosticsQuery {
        outcome: None,
        since: q.since,
        month: q.month,
        limit: None,
    }) {
        Ok(range) => range,
        Err(error) => return map_runtime_error(error),
    };
    match super::super::blocking("turns to first edit", move || {
        turns_to_edit_trend(&runtime, &range, &filters)
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
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

/// Turns and result bytes before the first edit, or `None` when no call is
/// a detectable edit. `None` is an exclusion, including an empty trace.
/// Each item is `(seq, tool_name, result_bytes)`.
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

struct SlimInvocation {
    id: i64,
    ts: DateTime<Utc>,
    agent: String,
    task_ids: Vec<String>,
    score: Option<(u64, u64)>,
}

struct FirstEdit {
    ts: DateTime<Utc>,
    id: i64,
    agent: String,
    score: Option<(u64, u64)>,
}

/// Page one activity newest-first. The store's default limit is 100, so
/// each page sets [`PAGE_LIMIT`]. A full page of one timestamp that yields
/// no new row stops the walk: older rows may remain, and `truncated` says so.
fn load_activity_invocations(
    runtime: &OrbitRuntime,
    activity: &str,
    since: Option<DateTime<Utc>>,
) -> Result<(Vec<SlimInvocation>, bool), orbit_core::OrbitError> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut until: Option<DateTime<Utc>> = None;
    loop {
        if out.len() >= MAX_RECORDS {
            return Ok((out, true));
        }
        let page = runtime.workspace_invocation_records(InvocationQuery {
            since,
            until,
            activity_id: Some(activity.to_string()),
            limit: PAGE_LIMIT,
            ..Default::default()
        })?;
        if page.is_empty() {
            return Ok((out, false));
        }
        let Some(boundary) = page.last() else {
            return Ok((out, false));
        };
        let oldest = boundary.ts;
        let page_len = page.len();
        let mut added = 0usize;
        let mut hit_cap = false;
        for record in page {
            if !seen.insert(record.id) {
                continue;
            }
            if out.len() >= MAX_RECORDS {
                hit_cap = true;
                break;
            }
            let score = turns_before_edit(
                record
                    .tool_calls
                    .iter()
                    .map(|call| (call.seq, call.tool_name.as_str(), call.result_bytes)),
            );
            out.push(SlimInvocation {
                id: record.id,
                ts: record.ts,
                agent: record.agent,
                task_ids: record.task_ids,
                score,
            });
            added += 1;
        }
        if hit_cap {
            return Ok((out, true));
        }
        if page_len < PAGE_LIMIT {
            return Ok((out, false));
        }
        if added == 0 {
            return Ok((out, true));
        }
        until = Some(oldest);
    }
}

/// What one task adds to the points it falls in.
#[derive(Clone, Copy)]
enum Contribution {
    Scored(u64, u64),
    /// The first implement invocation recorded no detectable edit.
    NoEdit,
    /// No implement invocation is linked to the task.
    NoInvocation,
}

/// The tasks one series point (or a series' window total) averages, and
/// those it leaves out.
#[derive(Clone, Default)]
struct Cell {
    samples: Vec<(u64, u64)>,
    no_edit: u64,
    no_invocation: u64,
}

impl Cell {
    fn record(&mut self, contribution: Contribution) {
        match contribution {
            Contribution::Scored(turns, bytes) => self.samples.push((turns, bytes)),
            Contribution::NoEdit => self.no_edit += 1,
            Contribution::NoInvocation => self.no_invocation += 1,
        }
    }

    fn is_empty(&self) -> bool {
        self.samples.is_empty() && self.no_edit == 0 && self.no_invocation == 0
    }

    fn to_json(&self) -> Value {
        let n = self.samples.len();
        let (mean_turns, median_turns) = average(self.samples.iter().map(|(turns, _)| *turns));
        let (mean_bytes, median_bytes) = average(self.samples.iter().map(|(_, bytes)| *bytes));
        json!({
            "n": n,
            "mean_turns": mean_turns,
            "median_turns": median_turns,
            "mean_result_bytes": mean_bytes,
            "median_result_bytes": median_bytes,
            "thin": n > 0 && n < MIN_TASKS,
            "excluded_no_edit": self.no_edit,
            "excluded_no_invocation": self.no_invocation,
        })
    }
}

/// Mean and median of `values`, or null when there are none. An even median
/// is the mean of the two central values.
fn average(values: impl Iterator<Item = u64>) -> (Value, Value) {
    let mut values: Vec<u64> = values.collect();
    let n = values.len();
    if n == 0 {
        return (Value::Null, Value::Null);
    }
    let sum: u128 = values.iter().map(|value| u128::from(*value)).sum();
    values.sort_unstable();
    let median = if n % 2 == 1 {
        values[n / 2] as f64
    } else {
        (values[n / 2 - 1] as f64 + values[n / 2] as f64) / 2.0
    };
    (json!(sum as f64 / n as f64), json!(median))
}

struct Series {
    points: Vec<Cell>,
    total: Cell,
}

fn turns_to_edit_trend(
    runtime: &OrbitRuntime,
    range: &DiagnosticsRange,
    filters: &Filters,
) -> Result<Value, orbit_core::OrbitError> {
    let transitions: Vec<_> = runtime
        .task_terminal_transitions(TrendWindow::read_since(range))?
        .into_iter()
        .filter(|row| !filters.done_only || row.status == TaskStatus::Done)
        .collect();
    let window = TrendWindow::new(range, transitions.first().map(|row| row.at));
    let in_window: Vec<_> = transitions
        .iter()
        .filter_map(|row| window.bucket_of(row.at).map(|index| (index, row)))
        .filter(|(_, row)| {
            filters
                .complexities
                .as_ref()
                .is_none_or(|selected| selected.contains(row.complexity.as_str()))
        })
        .collect();
    // No implement invocation predates its task, so the oldest task in the
    // window bounds the read, with a day's slack for clock skew. An owner
    // that omits `created_at` forces a full read.
    let read_since = in_window
        .iter()
        .map(|(_, row)| row.created_at)
        .collect::<Option<Vec<_>>>()
        .and_then(|created| created.into_iter().min())
        .map(|oldest| oldest - READ_SLACK);

    let mut first_by_task: HashMap<String, FirstEdit> = HashMap::new();
    let mut truncated = false;
    if !in_window.is_empty() {
        for activity in IMPLEMENT_INVOCATION_IDS {
            let (invocations, activity_truncated) =
                load_activity_invocations(runtime, activity, read_since)?;
            truncated |= activity_truncated;
            for invocation in invocations {
                for task_id in invocation.task_ids {
                    let replace = match first_by_task.get(&task_id) {
                        Some(existing) => {
                            (invocation.ts, invocation.id) < (existing.ts, existing.id)
                        }
                        None => true,
                    };
                    if replace {
                        first_by_task.insert(
                            task_id,
                            FirstEdit {
                                ts: invocation.ts,
                                id: invocation.id,
                                agent: invocation.agent.clone(),
                                score: invocation.score,
                            },
                        );
                    }
                }
            }
        }
    }

    let cohorts: &[&str] = if filters.tags.is_empty() {
        &["all"]
    } else {
        &["with", "without"]
    };
    let bucket_count = window.bucket_count();
    let mut series: BTreeMap<(String, &str), Series> = BTreeMap::new();
    let standing = STANDING_SERIES.iter().filter(|complexity| {
        filters
            .complexities
            .as_ref()
            .is_none_or(|selected| selected.contains(*complexity))
    });
    for complexity in standing {
        for cohort in cohorts {
            series.insert(
                (complexity.to_string(), cohort),
                Series {
                    points: vec![Cell::default(); bucket_count],
                    total: Cell::default(),
                },
            );
        }
    }
    let mut tag_counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut provider_counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut overall = Cell::default();
    let mut other_provider = 0u64;
    for (index, row) in &in_window {
        for tag in &row.tags {
            *tag_counts.entry(tag.as_str()).or_default() += 1;
        }
        let first = first_by_task.get(&row.task_id);
        if let Some(first) = first
            && !first.agent.is_empty()
        {
            *provider_counts.entry(first.agent.clone()).or_default() += 1;
        }
        if let Some(wanted) = filters.provider.as_deref()
            && let Some(first) = first
            && first.agent != wanted
        {
            other_provider += 1;
            continue;
        }
        let cohort = if filters.tags.is_empty() {
            "all"
        } else if row.tags.iter().any(|tag| filters.tags.contains(tag)) {
            "with"
        } else {
            "without"
        };
        let contribution = match first {
            None => Contribution::NoInvocation,
            Some(first) => match first.score {
                Some((turns, bytes)) => Contribution::Scored(turns, bytes),
                None => Contribution::NoEdit,
            },
        };
        let entry = series
            .entry((row.complexity.clone(), cohort))
            .or_insert_with(|| Series {
                points: vec![Cell::default(); bucket_count],
                total: Cell::default(),
            });
        entry.points[*index].record(contribution);
        entry.total.record(contribution);
        overall.record(contribution);
    }

    let mut series: Vec<_> = series.into_iter().collect();
    series.sort_by(|((left, left_cohort), _), ((right, right_cohort), _)| {
        complexity_bucket_ord(left)
            .cmp(&complexity_bucket_ord(right))
            .then_with(|| left_cohort.cmp(right_cohort))
    });
    let series: Vec<Value> = series
        .into_iter()
        .filter(|((complexity, _), series)| {
            STANDING_SERIES.contains(&complexity.as_str()) || !series.total.is_empty()
        })
        .map(|((complexity, cohort), series)| {
            json!({
                "complexity": complexity,
                "cohort": cohort,
                "points": series.points.iter().map(Cell::to_json).collect::<Vec<_>>(),
                "total": series.total.to_json(),
            })
        })
        .collect();
    let mut tags: Vec<(&str, u64)> = tag_counts.into_iter().collect();
    tags.sort_by(|(left, left_n), (right, right_n)| right_n.cmp(left_n).then(left.cmp(right)));
    tags.truncate(AVAILABLE_TAGS);
    let mut providers: Vec<(String, u64)> = provider_counts.into_iter().collect();
    providers.sort_by(|(left, left_n), (right, right_n)| {
        right_n.cmp(left_n).then_with(|| left.cmp(right))
    });

    Ok(json!({
        "window": window.to_json(),
        "outcome": if filters.done_only { "done" } else { "all" },
        "provider": filters.provider,
        "tags": filters.tags,
        "min_tasks": MIN_TASKS,
        "truncated": truncated,
        "bucket_starts": (0..bucket_count)
            .map(|index| window.bucket_start(index).to_rfc3339())
            .collect::<Vec<_>>(),
        "series": series,
        "total": overall.to_json(),
        "excluded_other_provider": other_provider,
        "available_tags": tags
            .into_iter()
            .map(|(tag, tasks)| json!({ "tag": tag, "tasks": tasks }))
            .collect::<Vec<_>>(),
        "available_providers": providers
            .into_iter()
            .map(|(provider, tasks)| json!({ "provider": provider, "tasks": tasks }))
            .collect::<Vec<_>>(),
    }))
}
