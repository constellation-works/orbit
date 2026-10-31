//! Escaped-regression rate against landed change size, one series per crew
//! reference as dispatched, so `opus@medium` and `opus@high` stay apart.
//! Display only: nothing here enters a score.
//!
//! The cohort is the landed tasks whose final implementation attempt counts
//! for implementers in the window, with a recorded landed diff size and a
//! full follow-up period behind their completion. A task regressed when any
//! `regression_from` link against it was observed by the end of that
//! period. Sizes fall in `floor(log2(1 + lines))` buckets, each with a
//! Wilson 95% interval.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::{Value, json};

use super::performance::{counted, tier};
use crate::application::agents::AgentFacts;
use crate::application::agents::CREW_UNKNOWN;
use crate::application::agents::facts::{Attempt, TaskFacts};

/// Days after completion a regression link is looked for, and the follow-up
/// a task needs before it counts.
const FOLLOW_UP_DAYS: i64 = 7;
/// Tasks a bucket needs before its rate is drawn.
const MIN_BUCKET_TASKS: usize = 10;
/// Tasks a series needs before it is drawn by default.
const MIN_SERIES_TASKS: usize = 30;
/// Outcomes under which a drawn bucket is marked sparse.
const SPARSE_OUTCOMES: usize = 5;
/// The two-sided 95% normal quantile.
const Z95: f64 = 1.959_963_984_540_054;

/// The tiers a cohort can be narrowed to.
pub(in crate::application::agents) const TIERS: &[&str] =
    &["low", "medium", "hard", "xhard", "unassessed"];

#[derive(Default)]
struct Series {
    models: BTreeSet<String>,
    buckets: BTreeMap<u32, (usize, usize)>,
}

/// The `floor(log2(1 + lines))` bucket of a diff size.
fn bucket(lines: u64) -> u32 {
    lines.saturating_add(1).ilog2()
}

/// A share to four decimals, a hundredth of a percentage point.
fn share(value: f64) -> Value {
    json!((value * 10_000.0).round() / 10_000.0)
}

/// The Wilson score interval of `k` successes in `n` trials.
fn wilson(k: usize, n: usize) -> (f64, f64) {
    let (k, n) = (k as f64, n as f64);
    let p = k / n;
    let z2 = Z95 * Z95;
    let denominator = 1.0 + z2 / n;
    let centre = (p + z2 / (2.0 * n)) / denominator;
    let half = Z95 * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / denominator;
    ((centre - half).max(0.0), (centre + half).min(1.0))
}

/// The size curves for the window starting at `since`, narrowed to one
/// tier when `complexity` names one.
pub(in crate::application::agents) fn size_curves(
    facts: &AgentFacts,
    since: DateTime<Utc>,
    complexity: Option<&str>,
) -> serde_json::Map<String, Value> {
    let mature_by = facts.computed_at - Duration::days(FOLLOW_UP_DAYS);
    let mut observed: BTreeMap<&str, Vec<DateTime<Utc>>> = BTreeMap::new();
    for link in &facts.regressions {
        observed
            .entry(link.culprit.as_str())
            .or_default()
            .push(link.at);
    }
    let mut tiers: BTreeMap<&str, usize> = TIERS.iter().map(|tier| (*tier, 0)).collect();
    let (mut immature, mut no_size) = (0, 0);
    let mut series: BTreeMap<String, Series> = BTreeMap::new();
    for task in &facts.tasks {
        let Some((attempt, landed_at)) = final_counted(task, since) else {
            continue;
        };
        if landed_at > mature_by {
            immature += 1;
            continue;
        }
        let Some(&lines) = facts.landed_lines.get(&task.id) else {
            no_size += 1;
            continue;
        };
        *tiers.entry(tier(task)).or_default() += 1;
        if complexity.is_some_and(|wanted| wanted != tier(task)) {
            continue;
        }
        let regressed = observed.get(task.id.as_str()).is_some_and(|ats| {
            ats.iter()
                .any(|at| *at <= landed_at + Duration::days(FOLLOW_UP_DAYS))
        });
        let crew = attempt.runner.crew.as_deref().unwrap_or(CREW_UNKNOWN);
        let entry = series.entry(crew.to_string()).or_default();
        if let Some(model) = &attempt.runner.model {
            entry.models.insert(model.clone());
        }
        let cell = entry.buckets.entry(bucket(lines)).or_default();
        cell.0 += 1;
        cell.1 += usize::from(regressed);
    }
    let mut rows: Vec<Value> = series
        .into_iter()
        .map(|(crew, series)| {
            let tasks: usize = series.buckets.values().map(|cell| cell.0).sum();
            let regressed: usize = series.buckets.values().map(|cell| cell.1).sum();
            let buckets: Vec<Value> = series
                .buckets
                .iter()
                .map(|(&index, &(n, k))| {
                    let (low, high) = wilson(k, n);
                    json!({
                        "bucket": index,
                        "lines_from": (1_u64 << index) - 1,
                        "lines_to": (1_u64 << (index + 1)) - 2,
                        "tasks": n,
                        "regressed": k,
                        "rate": share(k as f64 / n as f64),
                        "low": share(low),
                        "high": share(high),
                        "drawn": n >= MIN_BUCKET_TASKS,
                        "sparse": k < SPARSE_OUTCOMES,
                    })
                })
                .collect();
            json!({
                "crew": crew,
                "models": series.models,
                "tasks": tasks,
                "regressed": regressed,
                "default_selected": tasks >= MIN_SERIES_TASKS
                    && buckets.iter().any(|bucket| bucket["drawn"] == true),
                "buckets": buckets,
            })
        })
        .collect();
    rows.sort_by(|left, right| {
        right["tasks"]
            .as_u64()
            .cmp(&left["tasks"].as_u64())
            .then(left["crew"].as_str().cmp(&right["crew"].as_str()))
    });
    let mut payload = serde_json::Map::new();
    payload.insert("complexity".into(), json!(complexity));
    payload.insert(
        "complexities".into(),
        TIERS
            .iter()
            .map(|tier| json!({ "tier": tier, "tasks": tiers[tier] }))
            .collect(),
    );
    payload.insert("follow_up_days".into(), json!(FOLLOW_UP_DAYS));
    payload.insert(
        "mature_by".into(),
        json!(mature_by.to_rfc3339_opts(SecondsFormat::Secs, true)),
    );
    payload.insert("min_bucket_tasks".into(), json!(MIN_BUCKET_TASKS));
    payload.insert("min_series_tasks".into(), json!(MIN_SERIES_TASKS));
    payload.insert("sparse_outcomes".into(), json!(SPARSE_OUTCOMES));
    payload.insert(
        "excluded".into(),
        json!({ "immature": immature, "no_size": no_size }),
    );
    payload.insert("series".into(), Value::Array(rows));
    payload
}

/// The task's final attempt and completion time, when it landed and that
/// attempt counts for implementers in the window.
fn final_counted(task: &TaskFacts, since: DateTime<Utc>) -> Option<(&Attempt, DateTime<Utc>)> {
    let attempt = &task.attempts[task.final_attempt?];
    (task.landed && counted(attempt, since)).then_some((attempt, task.landed_at?))
}
