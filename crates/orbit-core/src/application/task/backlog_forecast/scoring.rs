//! The scoring pass: stored forecasts against what their tasks did.
//!
//! Each stored task is scored on its own (its actual hours, tokens and USD
//! after the forecast against its stored p50 and p90), and a forecast whose
//! tasks have all finished is scored as a whole (time to drain, total USD
//! and tokens). A task since rejected, archived or deleted, or whose
//! complexity changed (re-scoped), is excluded and counted; a forecast with
//! an exclusion is not scored as a whole, since its totals covered work that
//! no longer exists.
//!
//! Hits are kept per model version: a version's quantiles mean something
//! different from another's, so the top-level hit rates describe the
//! current version alone and `by_model_version` holds every version's own.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use orbit_store::contracts::StoredBacklogForecast;
use orbit_types::task::{Task, TaskStatus, complexity_bucket};
use serde::Deserialize;
use serde_json::{Value, json};

use super::ForecastRun;

/// One task as a stored forecast recorded it.
#[derive(Deserialize)]
struct StoredTask {
    id: String,
    complexity: String,
    #[serde(default)]
    forecast: Value,
}

const METRICS: [&str; 3] = ["hours", "tokens", "usd"];
const TOTALS: [&str; 3] = ["time_to_drain_hours", "tokens", "usd"];

/// How many actuals fell at or under p50 and p90.
#[derive(Default, Clone, Copy)]
struct Hits {
    n: u64,
    under_p50: u64,
    under_p90: u64,
}

impl Hits {
    fn add(&mut self, actual: f64, quantiles: &Value) {
        let (Some(p50), Some(p90)) = (
            quantiles.get("p50").and_then(Value::as_f64),
            quantiles.get("p90").and_then(Value::as_f64),
        ) else {
            return;
        };
        self.n += 1;
        self.under_p50 += u64::from(actual <= p50);
        self.under_p90 += u64::from(actual <= p90);
    }

    fn to_json(self) -> Value {
        let share = |hits: u64| (self.n > 0).then(|| hits as f64 / self.n as f64);
        json!({
            "n": self.n,
            "under_p50": share(self.under_p50),
            "under_p90": share(self.under_p90),
        })
    }
}

#[derive(Default)]
struct Excluded {
    rejected: u64,
    rescoped: u64,
    missing: u64,
    no_delivery: u64,
}

impl Excluded {
    fn total(&self) -> u64 {
        self.rejected + self.rescoped + self.missing + self.no_delivery
    }

    fn to_json(&self) -> Value {
        json!({
            "rejected": self.rejected,
            "rescoped": self.rescoped,
            "missing": self.missing,
            "no_delivery": self.no_delivery,
        })
    }
}

fn round(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// One model version's hit rates.
#[derive(Default)]
struct VersionHits {
    forecasts: u64,
    task: [Hits; 3],
    total: [Hits; 3],
}

impl VersionHits {
    fn task_level(&self) -> Value {
        json!({
            "hours": self.task[0].to_json(),
            "tokens": self.task[1].to_json(),
            "usd": self.task[2].to_json(),
        })
    }

    fn forecast_level(&self) -> Value {
        json!({
            "time_to_drain_hours": self.total[0].to_json(),
            "tokens": self.total[1].to_json(),
            "usd": self.total[2].to_json(),
        })
    }
}

/// Score `stored` (newest first) against the tasks' current records, their
/// done times and their invocations. The top-level hit rates are
/// `current_version`'s.
pub(super) fn score_backlog_forecasts(
    stored: &[StoredBacklogForecast],
    tasks: &HashMap<String, Task>,
    done_at: &HashMap<String, DateTime<Utc>>,
    runs: &HashMap<String, Vec<ForecastRun>>,
    current_version: u32,
) -> Value {
    let mut versions: BTreeMap<u32, VersionHits> = BTreeMap::new();
    let mut all_excluded = Excluded::default();
    let mut forecasts = Vec::new();
    for forecast in stored {
        let hits = versions.entry(forecast.model_version).or_default();
        hits.forecasts += 1;
        let stored_tasks: Vec<StoredTask> =
            serde_json::from_str(&forecast.tasks_json).unwrap_or_default();
        let result: Value = serde_json::from_str(&forecast.result_json).unwrap_or(Value::Null);
        let mut excluded = Excluded::default();
        let mut pending = 0u64;
        let mut done = 0u64;
        let mut last_done: Option<DateTime<Utc>> = None;
        let mut tokens_total = Some(0.0);
        let mut usd_total = Some(0.0);
        for stored_task in &stored_tasks {
            let Some(task) = tasks.get(&stored_task.id) else {
                excluded.missing += 1;
                continue;
            };
            if matches!(task.status, TaskStatus::Rejected | TaskStatus::Archived) {
                excluded.rejected += 1;
                continue;
            }
            if complexity_bucket(task.complexity.map(|c| c.as_str())) != stored_task.complexity {
                excluded.rescoped += 1;
                continue;
            }
            if task.status != TaskStatus::Done {
                pending += 1;
                continue;
            }
            let used = super::forecast_usage(
                runs.get(&stored_task.id).map_or(&[][..], Vec::as_slice),
                Some(forecast.forecast_at),
            );
            if used.invocations == 0 {
                excluded.no_delivery += 1;
                continue;
            }
            done += 1;
            let finished = done_at.get(&stored_task.id).copied().or(used.last_end);
            last_done = last_done.max(finished);
            tokens_total = tokens_total.zip(used.tokens).map(|(sum, one)| sum + one);
            usd_total = usd_total.zip(used.usd).map(|(sum, one)| sum + one);
            let actuals = [Some(used.hours), used.tokens, used.usd];
            for (index, metric) in METRICS.iter().enumerate() {
                if let Some(actual) = actuals[index] {
                    hits.task[index].add(actual, &stored_task.forecast[*metric]);
                }
            }
        }
        let settled = pending == 0 && done > 0;
        let whole = settled && excluded.total() == 0;
        let actual = settled.then(|| {
            let drain = last_done
                .map(|at| round((at - forecast.forecast_at).num_seconds().max(0) as f64 / 3600.0));
            json!({
                "time_to_drain_hours": drain,
                "tokens": tokens_total.map(round),
                "usd": usd_total.map(round),
            })
        });
        if whole && let Some(actual) = &actual {
            for (index, key) in TOTALS.iter().enumerate() {
                if let Some(value) = actual[*key].as_f64() {
                    hits.total[index].add(value, &result[*key]);
                }
            }
        }
        all_excluded.rejected += excluded.rejected;
        all_excluded.rescoped += excluded.rescoped;
        all_excluded.missing += excluded.missing;
        all_excluded.no_delivery += excluded.no_delivery;
        forecasts.push(json!({
            "forecast_at": forecast.forecast_at.to_rfc3339(),
            "model_version": forecast.model_version,
            "tasks": stored_tasks.len(),
            "done": done,
            "pending": pending,
            "excluded": excluded.to_json(),
            "settled": settled,
            "scored_whole": whole,
            "forecast": {
                "time_to_drain_hours": result["time_to_drain_hours"],
                "tokens": result["tokens"],
                "usd": result["usd"],
            },
            "actual": actual,
        }));
    }
    let current = versions.remove(&current_version).unwrap_or_default();
    let mut by_model_version: Vec<Value> = versions
        .iter()
        .map(|(version, hits)| (*version, hits))
        .chain(std::iter::once((current_version, &current)))
        .filter(|(_, hits)| hits.forecasts > 0)
        .map(|(version, hits)| {
            json!({
                "model_version": version,
                "forecasts": hits.forecasts,
                "task_level": hits.task_level(),
                "forecast_level": hits.forecast_level(),
            })
        })
        .collect();
    by_model_version.sort_by_key(|row| std::cmp::Reverse(row["model_version"].as_u64()));
    json!({
        "model_version": current_version,
        "forecasts": forecasts,
        "task_level": current.task_level(),
        "forecast_level": current.forecast_level(),
        "by_model_version": by_model_version,
        "excluded": all_excluded.to_json(),
    })
}
