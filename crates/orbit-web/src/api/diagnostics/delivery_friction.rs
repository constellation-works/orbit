//! Delivery-friction events per trend bucket, as counts and as rates per
//! task done in the bucket, for Analytics › Metrics [ORB-18384].
//!
//! Every family is read at query time from records Orbit already keeps; no
//! table holds these series. Each family counts one distinct occurrence
//! once, at the time below:
//!
//! | Family | Source | Counted | Dedup |
//! |---|---|---|---|
//! | `step_recovery` | v2 audit `step_recovery_attempted` envelope events whose `recovery_activity` is not `pr_conflict_recovery` | one per recovery dispatch, at the event time; split by whether the recovery succeeded | envelope `event_id` |
//! | `final_recovery` | v2 audit `final_recovery_attempted` envelope events, `skipped` outcomes left out | one per hook that ran, at the event time; split by outcome (`resume`, `settled`, `escalated`) | envelope `event_id` |
//! | `auto_fix` | task history `pr_auto_fix_admitted` ledger entries on each pull request's primary task | one per admitted repair dispatch, at its admission; split by blocker | task, pull request, head and blocker |
//! | `merge_conflict` | v2 audit `step_recovery_attempted` envelope events whose `recovery_activity` is `pr_conflict_recovery` | one per conflict recovery dispatch, at the event time; split by whether it succeeded | envelope `event_id` |
//! | `failed_ci` | task history `pr_auto_fix_admitted` and `pr_auto_fix_out_of_run` entries whose blocker is `failed_checks`, and tasks tagged `ci-failure-sweep` | one per pull-request head with a failed required check, at its first observation, and one per sweep filing, at the task's `created_at` | pull request and head; sweep filings by task |
//! | `cost` | invocations' `provider_cost_usd` | USD summed per invocation, at its time; invocations without a valid provider cost are reported, not counted | invocation |
//! | `tokens` | invocations' normalized token total, as `GET /api/cost` normalizes it | tokens summed per invocation, at its time; invocations of an uncovered model are reported, not counted | invocation |
//!
//! Failed checks are recorded only where the `--auto-fix` watcher read the
//! pull request, so `failed_ci` misses failures on runs without it.
//!
//! The denominator of every rate is the tasks that reached `done` in the
//! bucket, dated as `/diagnostics/completions` dates them. A bucket with
//! fewer than [`MIN_DONE`] done tasks is marked sparse.
//!
//! The complexity and crew filters narrow the denominator and the families
//! whose occurrences belong to a task (`auto_fix`, `failed_ci`, `cost`,
//! `tokens`), by the task's current complexity and crew. An invocation
//! linked to several tasks counts when any of them matches. Audit events
//! name a run, not a task, so the recovery and conflict families ignore the
//! filters; their rates keep the unfiltered denominator, and the payload
//! says so per family.
//!
//! History is read only for tasks the index dates at or after the window's
//! read start: every history append updates the task, so an older task holds
//! no entry inside the window.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use axum::extract::Query;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use orbit_core::{OrbitRuntime, V2AuditEventFilter};
use orbit_types::task::{Task, TaskStatus, UNSET_BUCKET, complexity_bucket, complexity_bucket_ord};
use orbit_types::workflow::{PR_AUTO_FIX_ADMITTED_EVENT, PrAutoFixAttempt, PrAutoFixBlocker};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::super::{DiagnosticsQuery, map_runtime_error};
use super::metrics::DiagnosticsRange;
use super::trend::{TrendWindow, trend_range};
use crate::state::Ws;

/// A bucket whose denominator is below this many done tasks is sparse.
const MIN_DONE: u64 = 3;
/// Complexities a request may name; `unassessed` is an alias of the unset
/// bucket.
const COMPLEXITIES: [&str; 5] = ["low", "medium", "hard", "xhard", UNSET_BUCKET];
/// The most crews one request may name.
const MAX_CREWS: usize = 16;
/// Audit rows read per page.
const AUDIT_PAGE: usize = 2_000;
/// The task history event of an auto-fix a drain or the dashboard started
/// outside the run; it carries the head and blocker it repairs.
const AUTO_FIX_OUT_OF_RUN_EVENT: &str = "pr_auto_fix_out_of_run";
/// The tag the CI-failure sweep files its repair tasks under.
const CI_FAILURE_SWEEP_TAG: &str = "ci-failure-sweep";
/// The step recovery activity that resolves rebase conflicts.
const CONFLICT_RECOVERY_ACTIVITY: &str = "pr_conflict_recovery";

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Family {
    StepRecovery,
    FinalRecovery,
    AutoFix,
    MergeConflict,
    FailedCi,
    Cost,
    Tokens,
}

const FAMILIES: [Family; 7] = [
    Family::StepRecovery,
    Family::FinalRecovery,
    Family::AutoFix,
    Family::MergeConflict,
    Family::FailedCi,
    Family::Cost,
    Family::Tokens,
];

impl Family {
    fn key(self) -> &'static str {
        match self {
            Self::StepRecovery => "step_recovery",
            Self::FinalRecovery => "final_recovery",
            Self::AutoFix => "auto_fix",
            Self::MergeConflict => "merge_conflict",
            Self::FailedCi => "failed_ci",
            Self::Cost => "cost",
            Self::Tokens => "tokens",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::StepRecovery => "Step recoveries",
            Self::FinalRecovery => "Final recoveries",
            Self::AutoFix => "PR auto-fixes",
            Self::MergeConflict => "Merge conflicts",
            Self::FailedCi => "Failed CI",
            Self::Cost => "Cost",
            Self::Tokens => "Tokens",
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Self::Cost => "usd",
            Self::Tokens => "tokens",
            _ => "events",
        }
    }

    /// Whether each occurrence belongs to a task the filters can judge.
    fn task_scoped(self) -> bool {
        !matches!(
            self,
            Self::StepRecovery | Self::FinalRecovery | Self::MergeConflict
        )
    }

    fn reads_audit(self) -> bool {
        !self.task_scoped()
    }

    fn reads_history(self) -> bool {
        matches!(self, Self::AutoFix | Self::FailedCi)
    }

    fn reads_invocations(self) -> bool {
        matches!(self, Self::Cost | Self::Tokens)
    }
}

#[derive(Deserialize, Default)]
pub(in crate::api) struct DeliveryFrictionQuery {
    #[serde(default)]
    since: Option<String>,
    #[serde(default)]
    month: Option<String>,
    /// Comma-separated families; every one when absent.
    #[serde(default)]
    families: Option<String>,
    /// Comma-separated complexities; every one when absent.
    #[serde(default)]
    complexity: Option<String>,
    /// Comma-separated crews; every one when absent.
    #[serde(default)]
    crew: Option<String>,
}

struct Filters {
    families: Vec<Family>,
    complexities: Option<BTreeSet<&'static str>>,
    crews: Option<BTreeSet<String>>,
}

impl Filters {
    fn active(&self) -> bool {
        self.complexities.is_some() || self.crews.is_some()
    }

    fn admits(&self, task: Option<&Task>) -> bool {
        if !self.active() {
            return true;
        }
        let Some(task) = task else {
            return false;
        };
        let complexity = complexity_bucket(task.complexity.map(|c| c.as_str()));
        self.complexities
            .as_ref()
            .is_none_or(|selected| selected.contains(complexity))
            && self.crews.as_ref().is_none_or(|selected| {
                selected.contains(task.crew.as_deref().unwrap_or(UNKNOWN_CREW))
            })
    }
}

/// The crew filter's name for a task without a recorded crew.
const UNKNOWN_CREW: &str = "unknown";

fn comma_list(raw: Option<&str>) -> Vec<&str> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect()
}

fn parse_filters(q: &DeliveryFrictionQuery) -> Result<Filters, String> {
    let requested = comma_list(q.families.as_deref());
    let families = if requested.is_empty() {
        FAMILIES.to_vec()
    } else {
        let mut set = BTreeSet::new();
        for value in requested {
            let Some(family) = FAMILIES.iter().find(|family| family.key() == value) else {
                return Err(format!(
                    "unknown metric family '{value}': expected {}",
                    FAMILIES.map(Family::key).join(", ")
                ));
            };
            set.insert(*family);
        }
        set.into_iter().collect()
    };
    let requested = comma_list(q.complexity.as_deref());
    let complexities = if requested.is_empty() {
        None
    } else {
        let mut set = BTreeSet::new();
        for value in requested {
            let bucket = if value == "unassessed" {
                UNSET_BUCKET
            } else {
                value
            };
            let Some(known) = COMPLEXITIES.iter().find(|known| **known == bucket) else {
                return Err(format!(
                    "unknown complexity '{value}': expected low, medium, hard, xhard or unassessed"
                ));
            };
            set.insert(*known);
        }
        Some(set)
    };
    let requested = comma_list(q.crew.as_deref());
    if requested.len() > MAX_CREWS {
        return Err(format!("too many crews: at most {MAX_CREWS} may be named"));
    }
    let crews =
        (!requested.is_empty()).then(|| requested.into_iter().map(str::to_string).collect());
    Ok(Filters {
        families,
        complexities,
        crews,
    })
}

pub(in crate::api) async fn diagnostics_delivery_friction(
    Ws(runtime): Ws,
    Query(q): Query<DeliveryFrictionQuery>,
) -> Response {
    let filters = match parse_filters(&q) {
        Ok(filters) => filters,
        Err(message) => {
            return map_runtime_error(orbit_core::OrbitError::InvalidInput(message));
        }
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
    match super::super::blocking("delivery friction trend", move || {
        delivery_friction_trend(&runtime, &range, &filters)
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(response) => *response,
    }
}

/// One counted occurrence: when, how much, which tasks it belongs to, and
/// the breakdown key it adds to.
struct Occurrence {
    family: Family,
    at: DateTime<Utc>,
    value: f64,
    task_ids: Vec<String>,
    part: &'static str,
}

/// One point (or a window total) of one family.
#[derive(Clone, Default)]
struct Cell {
    count: f64,
    parts: BTreeMap<&'static str, f64>,
    /// Invocations left out of `count`: no valid cost, or an uncovered model.
    excluded: u64,
}

impl Cell {
    fn add(&mut self, value: f64, part: &'static str) {
        self.count += value;
        *self.parts.entry(part).or_default() += value;
    }

    fn to_json(&self, family: Family, denominator: u64) -> Value {
        let rate = (denominator > 0).then(|| self.count / denominator as f64);
        let parts: Map<String, Value> = self
            .parts
            .iter()
            .map(|(key, value)| ((*key).to_string(), amount(family, *value)))
            .collect();
        let mut cell = json!({
            "count": amount(family, self.count),
            "denominator": denominator,
            "rate": rate,
            "sparse": denominator < MIN_DONE,
            "breakdown": parts,
        });
        if family.reads_invocations() {
            cell["excluded_invocations"] = json!(self.excluded);
        }
        cell
    }
}

/// USD to four decimals; events and tokens are whole.
fn amount(family: Family, value: f64) -> Value {
    if family == Family::Cost {
        json!((value * 10_000.0).round() / 10_000.0)
    } else {
        json!(value.round() as u64)
    }
}

struct Series {
    points: Vec<Cell>,
    total: Cell,
    previous: Cell,
}

/// Done tasks per bucket, in the window and in the previous one.
struct Denominator {
    points: Vec<u64>,
    total: u64,
    previous: u64,
}

impl Denominator {
    fn new(buckets: usize) -> Self {
        Self {
            points: vec![0; buckets],
            total: 0,
            previous: 0,
        }
    }
}

fn delivery_friction_trend(
    runtime: &OrbitRuntime,
    range: &DiagnosticsRange,
    filters: &Filters,
) -> Result<Value, orbit_core::OrbitError> {
    let read_since = TrendWindow::read_since(range);
    let done: Vec<_> = runtime
        .task_terminal_transitions(read_since)?
        .into_iter()
        .filter(|row| row.status == TaskStatus::Done)
        .collect();
    let needs_tasks =
        filters.active() || filters.families.iter().any(|family| family.reads_history());
    // The crew picker reads the done tasks' crews in every request, so the
    // listing is narrowed to those ids unless a filter or history scan needs
    // the whole listing.
    let metadata = if needs_tasks {
        runtime.list_task_metadata()?
    } else {
        let ids: BTreeSet<String> = done.iter().map(|row| row.task_id.clone()).collect();
        runtime.list_task_metadata_for_ids(&ids)?
    };
    let tasks: HashMap<String, Task> = metadata
        .into_iter()
        .map(|task| (task.id.to_string(), task))
        .collect();

    let mut occurrences = Vec::new();
    if filters.families.iter().any(|family| family.reads_audit()) {
        occurrences.extend(audit_occurrences(runtime, read_since, &filters.families)?);
    }
    if filters.families.iter().any(|family| family.reads_history()) {
        occurrences.extend(history_occurrences(
            runtime,
            read_since,
            &tasks,
            &filters.families,
        )?);
    }
    let mut excluded: Vec<(Family, DateTime<Utc>, Vec<String>)> = Vec::new();
    if filters
        .families
        .iter()
        .any(|family| family.reads_invocations())
    {
        for spend in runtime.invocation_spend(read_since)? {
            for family in [Family::Cost, Family::Tokens] {
                if !filters.families.contains(&family) {
                    continue;
                }
                let value = match family {
                    Family::Cost => spend.provider_cost_usd,
                    _ => spend.normalized_tokens.map(|tokens| tokens as f64),
                };
                match value {
                    Some(value) => occurrences.push(Occurrence {
                        family,
                        at: spend.ts,
                        value,
                        task_ids: spend.task_ids.clone(),
                        part: "invocations",
                    }),
                    None => excluded.push((family, spend.ts, spend.task_ids.clone())),
                }
            }
        }
    }

    // `all` windows open at the oldest sample any source read.
    let earliest = done
        .iter()
        .map(|row| row.at)
        .chain(occurrences.iter().map(|occurrence| occurrence.at))
        .min();
    let window = TrendWindow::new(range, earliest);
    let bucket_count = window.bucket_count();

    let mut filtered = Denominator::new(bucket_count);
    let mut unfiltered = Denominator::new(bucket_count);
    for row in &done {
        let admitted = filters.admits(tasks.get(&row.task_id));
        for (denominator, counts) in [(&mut unfiltered, true), (&mut filtered, admitted)] {
            if !counts {
                continue;
            }
            if let Some(index) = window.bucket_of(row.at) {
                denominator.points[index] += 1;
                denominator.total += 1;
            } else if window.in_previous(row.at) {
                denominator.previous += 1;
            }
        }
    }

    let admits = |family: Family, task_ids: &[String]| {
        !family.task_scoped()
            || !filters.active()
            || task_ids.iter().any(|id| filters.admits(tasks.get(id)))
    };
    let mut series: BTreeMap<Family, Series> = filters
        .families
        .iter()
        .map(|family| {
            (
                *family,
                Series {
                    points: vec![Cell::default(); bucket_count],
                    total: Cell::default(),
                    previous: Cell::default(),
                },
            )
        })
        .collect();
    for occurrence in &occurrences {
        if !admits(occurrence.family, &occurrence.task_ids) {
            continue;
        }
        let Some(entry) = series.get_mut(&occurrence.family) else {
            continue;
        };
        if let Some(index) = window.bucket_of(occurrence.at) {
            entry.points[index].add(occurrence.value, occurrence.part);
            entry.total.add(occurrence.value, occurrence.part);
        } else if window.in_previous(occurrence.at) {
            entry.previous.add(occurrence.value, occurrence.part);
        }
    }
    for (family, at, task_ids) in &excluded {
        if !admits(*family, task_ids) {
            continue;
        }
        let Some(entry) = series.get_mut(family) else {
            continue;
        };
        if let Some(index) = window.bucket_of(*at) {
            entry.points[index].excluded += 1;
            entry.total.excluded += 1;
        } else if window.in_previous(*at) {
            entry.previous.excluded += 1;
        }
    }

    let families: Vec<Value> = series
        .iter()
        .map(|(family, entry)| {
            let applies = family.task_scoped() && filters.active();
            let denominator = if family.task_scoped() {
                &filtered
            } else {
                &unfiltered
            };
            json!({
                "family": family.key(),
                "label": family.label(),
                "unit": family.unit(),
                "task_scoped": family.task_scoped(),
                "filtered": applies,
                "points": entry
                    .points
                    .iter()
                    .zip(&denominator.points)
                    .map(|(cell, done)| cell.to_json(*family, *done))
                    .collect::<Vec<_>>(),
                "total": entry.total.to_json(*family, denominator.total),
                "previous": window
                    .previous
                    .map(|_| entry.previous.to_json(*family, denominator.previous)),
            })
        })
        .collect();

    // Filter choices: the crews and complexities of the window's done tasks.
    let mut crews: BTreeMap<&str, u64> = BTreeMap::new();
    let mut complexities: BTreeMap<&str, u64> = BTreeMap::new();
    for row in &done {
        if window.bucket_of(row.at).is_none() {
            continue;
        }
        *complexities.entry(row.complexity.as_str()).or_default() += 1;
        if let Some(task) = tasks.get(&row.task_id) {
            *crews
                .entry(task.crew.as_deref().unwrap_or(UNKNOWN_CREW))
                .or_default() += 1;
        }
    }
    let mut crews: Vec<(&str, u64)> = crews.into_iter().collect();
    crews.sort_by(|(left, left_n), (right, right_n)| right_n.cmp(left_n).then(left.cmp(right)));
    let mut complexities: Vec<(&str, u64)> = complexities.into_iter().collect();
    complexities.sort_by_key(|(complexity, _)| complexity_bucket_ord(complexity));

    Ok(json!({
        "window": window.to_json(),
        "families": families,
        "complexity": filters.complexities.as_ref().map(|set| set.iter().collect::<Vec<_>>()),
        "crew": filters.crews.as_ref().map(|set| set.iter().collect::<Vec<_>>()),
        "min_done": MIN_DONE,
        "bucket_starts": (0..bucket_count)
            .map(|index| window.bucket_start(index).to_rfc3339())
            .collect::<Vec<_>>(),
        "done": {
            "points": filtered.points,
            "total": filtered.total,
            "previous": window.previous.map(|_| filtered.previous),
            "unfiltered_points": unfiltered.points,
            "unfiltered_total": unfiltered.total,
        },
        "available_crews": crews
            .into_iter()
            .map(|(crew, tasks)| json!({ "crew": crew, "tasks": tasks }))
            .collect::<Vec<_>>(),
        "available_complexities": complexities
            .into_iter()
            .map(|(complexity, tasks)| json!({ "complexity": complexity, "tasks": tasks }))
            .collect::<Vec<_>>(),
    }))
}

/// The recovery and conflict families from v2 audit envelope events.
fn audit_occurrences(
    runtime: &OrbitRuntime,
    since: Option<DateTime<Utc>>,
    families: &[Family],
) -> Result<Vec<Occurrence>, orbit_core::OrbitError> {
    let mut body_kinds = Vec::new();
    if families.contains(&Family::StepRecovery) || families.contains(&Family::MergeConflict) {
        body_kinds.push("step_recovery_attempted".to_string());
    }
    if families.contains(&Family::FinalRecovery) {
        body_kinds.push("final_recovery_attempted".to_string());
    }
    let mut seen = HashSet::new();
    let mut occurrences = Vec::new();
    let mut offset = 0;
    loop {
        let page = OrbitRuntime::list_v2_audit_events(
            runtime,
            V2AuditEventFilter {
                workspace_id: String::new(),
                since,
                source: Some("v2_envelope".to_string()),
                body_kinds: body_kinds.clone(),
                limit: Some(AUDIT_PAGE),
                offset: Some(offset),
                oldest_first: true,
                ..Default::default()
            },
        )?;
        let fetched = page.len();
        offset += fetched;
        for row in page {
            let Ok(payload) = serde_json::from_str::<Value>(&row.payload_json) else {
                continue;
            };
            let field = |name: &str| payload.get(name).and_then(Value::as_str);
            let Some(event_id) = field("event_id") else {
                continue;
            };
            if !seen.insert(event_id.to_string()) {
                continue;
            }
            let occurrence = match field("body_kind") {
                Some("step_recovery_attempted") => {
                    let family = if field("recovery_activity") == Some(CONFLICT_RECOVERY_ACTIVITY) {
                        Family::MergeConflict
                    } else {
                        Family::StepRecovery
                    };
                    let succeeded = payload
                        .get("recovery_succeeded")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    Occurrence {
                        family,
                        at: row.ts,
                        value: 1.0,
                        task_ids: Vec::new(),
                        part: if succeeded { "recovered" } else { "failed" },
                    }
                }
                Some("final_recovery_attempted") => {
                    let part = match field("outcome") {
                        Some("skipped") => continue,
                        Some("resume") => "resume",
                        Some("settled") => "settled",
                        Some("escalated") => "escalated",
                        _ => "other",
                    };
                    Occurrence {
                        family: Family::FinalRecovery,
                        at: row.ts,
                        value: 1.0,
                        task_ids: Vec::new(),
                        part,
                    }
                }
                _ => continue,
            };
            if families.contains(&occurrence.family) {
                occurrences.push(occurrence);
            }
        }
        if fetched < AUDIT_PAGE {
            break;
        }
    }
    Ok(occurrences)
}

/// The head and blocker an out-of-run auto-fix start names.
#[derive(Deserialize)]
struct OutOfRunStart {
    pr_number: String,
    head_sha: String,
    blocker: PrAutoFixBlocker,
}

/// The auto-fix and failed-CI families from task history and sweep filings.
fn history_occurrences(
    runtime: &OrbitRuntime,
    since: Option<DateTime<Utc>>,
    tasks: &HashMap<String, Task>,
    families: &[Family],
) -> Result<Vec<Occurrence>, orbit_core::OrbitError> {
    let mut occurrences = Vec::new();
    // Failed heads, first observation kept: (pr, head) → (at, task).
    let mut failed_heads: BTreeMap<(String, String), (DateTime<Utc>, String)> = BTreeMap::new();
    let mut admitted = HashSet::new();
    let mut ids: Vec<&String> = tasks
        .iter()
        .filter(|(_, task)| since.is_none_or(|since| task.updated_at >= since))
        .map(|(id, _)| id)
        .collect();
    ids.sort();
    for id in ids {
        let history = match runtime.get_task_history(id) {
            Ok(history) => history,
            Err(error) => {
                tracing::warn!(task_id = %id, %error, "delivery friction: task history unreadable");
                continue;
            }
        };
        for entry in &history {
            if since.is_some_and(|since| entry.at < since) {
                continue;
            }
            let Some(note) = entry.note.as_deref() else {
                continue;
            };
            let (pr_number, head_sha, blocker) = if entry.event == PR_AUTO_FIX_ADMITTED_EVENT {
                let Ok(attempt) = serde_json::from_str::<PrAutoFixAttempt>(note) else {
                    continue;
                };
                let key = (
                    id.clone(),
                    attempt.pr_number.clone(),
                    attempt.head_sha.clone(),
                    attempt.blocker.as_str(),
                );
                if families.contains(&Family::AutoFix) && admitted.insert(key) {
                    occurrences.push(Occurrence {
                        family: Family::AutoFix,
                        at: entry.at,
                        value: 1.0,
                        task_ids: vec![id.clone()],
                        part: attempt.blocker.as_str(),
                    });
                }
                (attempt.pr_number, attempt.head_sha, attempt.blocker)
            } else if entry.event == AUTO_FIX_OUT_OF_RUN_EVENT {
                let Ok(start) = serde_json::from_str::<OutOfRunStart>(note) else {
                    continue;
                };
                (start.pr_number, start.head_sha, start.blocker)
            } else {
                continue;
            };
            if blocker == PrAutoFixBlocker::FailedChecks {
                let first = failed_heads
                    .entry((pr_number, head_sha))
                    .or_insert_with(|| (entry.at, id.clone()));
                if entry.at < first.0 {
                    *first = (entry.at, id.clone());
                }
            }
        }
    }
    if families.contains(&Family::FailedCi) {
        for (at, task_id) in failed_heads.into_values() {
            occurrences.push(Occurrence {
                family: Family::FailedCi,
                at,
                value: 1.0,
                task_ids: vec![task_id],
                part: "pr_head",
            });
        }
        for (id, task) in tasks {
            if since.is_some_and(|since| task.created_at < since)
                || !task.tags.iter().any(|tag| tag == CI_FAILURE_SWEEP_TAG)
            {
                continue;
            }
            occurrences.push(Occurrence {
                family: Family::FailedCi,
                at: task.created_at,
                value: 1.0,
                task_ids: vec![id.clone()],
                part: "sweep_filing",
            });
        }
    }
    Ok(occurrences)
}
