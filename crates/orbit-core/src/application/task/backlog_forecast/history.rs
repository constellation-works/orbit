use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{BacklogForecastStoreBackend as _, StoredBacklogForecast};
use orbit_types::task::{Task, TaskHistoryEntry, TaskRelationType, TaskStatus};
use serde_json::Value;

use crate::OrbitRuntime;

use super::assembly::ARRIVAL_DAYS;
use super::options::{DEFAULT_LOOKBACK_DAYS, MIN_HISTORY_TASKS, Options};
use super::outcomes::TaskEpisodes;
use super::persistence::SCORED_FORECASTS;
use super::{outcomes, scoring};

pub(super) struct LoadedForecastHistory {
    pub(super) tasks: HashMap<String, Task>,
    pub(super) stored: Vec<StoredBacklogForecast>,
    pub(super) done_at: HashMap<String, DateTime<Utc>>,
    pub(super) task_episodes: HashMap<String, TaskEpisodes>,
    pub(super) history_rows: Vec<orbit_store::contracts::TaskTerminalTransition>,
    pub(super) lookback_start: DateTime<Utc>,
    pub(super) lookback_days: i64,
    pub(super) min_history: usize,
    pub(super) arrival_start: DateTime<Utc>,
    pub(super) oldest_forecast: Option<DateTime<Utc>>,
    pub(super) return_start: DateTime<Utc>,
    pub(super) attempted: bool,
}
/// Task ids on stored model-10 forecasts. Their retry-inclusive actual is
/// read from status episodes even after they leave the return window.
fn attempt_cohort_ids(stored: &[StoredBacklogForecast]) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    for forecast in stored {
        if forecast.model_version < scoring::ATTEMPT_USAGE_VERSION {
            continue;
        }
        let Ok(rows) = serde_json::from_str::<Vec<Value>>(&forecast.tasks_json) else {
            continue;
        };
        for row in rows {
            if let Some(id) = row.get("id").and_then(Value::as_str) {
                ids.insert(id.to_string());
            }
        }
    }
    ids
}

/// Read `id`'s status history once per forecast; `None` when unreadable.
fn cached_history<'a>(
    runtime: &OrbitRuntime,
    cache: &'a mut HashMap<String, Option<Vec<TaskHistoryEntry>>>,
    id: &str,
) -> Option<&'a [TaskHistoryEntry]> {
    cache
        .entry(id.to_string())
        .or_insert_with(|| match runtime.get_task_history(id) {
            Ok(history) => Some(history),
            Err(error) => {
                tracing::warn!(task_id = %id, %error, "backlog forecast: task history unreadable");
                None
            }
        })
        .as_deref()
}

pub(super) fn load_forecast_history(
    runtime: &OrbitRuntime,
    options: &Options,
    now: DateTime<Utc>,
) -> Result<LoadedForecastHistory, OrbitError> {
    let day_start = options
        .history_tasks
        .is_none()
        .then(|| now - Duration::days(options.lookback_days));
    let min_history = options.history_tasks.unwrap_or(MIN_HISTORY_TASKS);
    let arrival_start = now - Duration::days(ARRIVAL_DAYS);
    let tasks: HashMap<String, Task> = runtime
        .list_task_metadata()?
        .into_iter()
        .map(|task| (task.id.to_string(), task))
        .collect();
    let recent = runtime.backlog_forecasts(SCORED_FORECASTS)?;
    let mature = runtime.sqlite_store()?.mature_backlog_forecasts(
        &runtime.workspace_id()?,
        now - Duration::days(90),
        9,
        SCORED_FORECASTS,
    )?;
    let mut stored = recent;
    for row in mature {
        if !stored
            .iter()
            .any(|existing| existing.forecast_at == row.forecast_at)
        {
            stored.push(row);
        }
    }
    stored.sort_by_key(|row| std::cmp::Reverse(row.forecast_at));
    let oldest_forecast = stored.iter().map(|forecast| forecast.forecast_at).min();
    let transitions_since = [day_start, Some(arrival_start), oldest_forecast]
        .into_iter()
        .flatten()
        .min();
    let mut done_at: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut completions = Vec::new();
    let mut histories: HashMap<String, Option<Vec<TaskHistoryEntry>>> = HashMap::new();
    // The index dates terminal rows by updated_at, which later comments or
    // artifacts can advance. Use it only to narrow candidates: an actual
    // completion cannot be newer than its last update. Read each candidate's
    // event log once, using the latest entry into done (including creation
    // in done) for both history membership and calibration. Never substitute
    // mutable metadata when an older task has no recorded done event.
    let completed_at = |runtime: &OrbitRuntime,
                        histories: &mut HashMap<String, Option<Vec<TaskHistoryEntry>>>,
                        task_id: &str| {
        cached_history(runtime, histories, task_id).and_then(|task_history| {
            task_history
                .iter()
                .rev()
                .find(|entry| entry.to_status == Some(TaskStatus::Done))
                .map(|entry| entry.at)
        })
    };
    for row in runtime
        .task_terminal_transitions(transitions_since)?
        .into_iter()
        .filter(|row| row.status == TaskStatus::Done)
    {
        if let Some(at) = completed_at(runtime, &mut histories, &row.task_id) {
            done_at.insert(row.task_id.clone(), at);
            completions.push((at, row));
        }
    }
    // Count floor: too few completions inside the day lookback (or a task
    // count was asked for) reaches back, newest first, until the newest
    // `min_history` completions are known. Rows dated before the
    // `min_history`-th newest completion cannot complete after it, so the
    // walk stops there.
    let in_days = completions
        .iter()
        .filter(|(at, _)| day_start.is_none_or(|start| *at >= start))
        .count();
    if in_days < min_history {
        let mut older: Vec<_> = runtime
            .task_terminal_transitions(None)?
            .into_iter()
            .filter(|row| {
                row.status == TaskStatus::Done
                    && transitions_since.is_some_and(|since| row.at < since)
            })
            .collect();
        older.sort_by_key(|row| std::cmp::Reverse(row.at));
        for row in older {
            let mut newest: Vec<DateTime<Utc>> = completions.iter().map(|(at, _)| *at).collect();
            newest.sort_unstable_by_key(|at| std::cmp::Reverse(*at));
            if newest.get(min_history - 1).is_some_and(|nth| row.at < *nth) {
                break;
            }
            if let Some(at) = completed_at(runtime, &mut histories, &row.task_id) {
                done_at.insert(row.task_id.clone(), at);
                completions.push((at, row));
            }
        }
    }
    completions.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    let counted_start = completions
        .get(min_history.min(completions.len()).saturating_sub(1))
        .map(|(at, _)| *at);
    let lookback_start = match (day_start, counted_start) {
        // Too few completions in the days: reach back to the counted ones.
        (Some(days), Some(counted)) if in_days < min_history => days.min(counted),
        (Some(days), _) => days,
        (None, Some(counted)) => counted,
        (None, None) => now - Duration::days(DEFAULT_LOOKBACK_DAYS),
    };
    let mut history_rows: Vec<_> = completions
        .into_iter()
        .filter(|(at, _)| *at >= lookback_start)
        .map(|(_, row)| row)
        .collect();
    if let Some(tasks) = options.history_tasks {
        history_rows.truncate(tasks);
    }
    // Index order, oldest first, as the day lookback has always read them.
    history_rows.sort_by_key(|row| row.at);
    // The span the lookback covers, which the return window extends.
    let lookback_days = (now - lookback_start).num_days().max(1);

    // Attempts and returns come from status history. Returns are observed
    // from 90 days before the lookback, so a recent lookback need not prove
    // a 90-day tail on its own. `updated_at` only narrows the candidates: a
    // transition cannot be newer than its task's last update. Running and
    // blocked tasks are always read, for their elapsed age.
    //
    // Model 10 scores a stored cohort's retry-inclusive work from the same
    // episodes. A retained snapshot can outlive this window while the task's
    // `updated_at` stays at completion, so every stored model-10 member with
    // a current record is read too. Outcome and return samples below still
    // admit only events inside their own windows.
    let return_start = lookback_start - Duration::days(outcomes::RETURN_WINDOW_DAYS);
    let mut episode_ids: BTreeSet<String> = tasks
        .iter()
        .filter(|(_, task)| {
            task.updated_at >= return_start
                || matches!(task.status, TaskStatus::InProgress | TaskStatus::Blocked)
        })
        .map(|(id, _)| id.clone())
        .collect();
    episode_ids.extend(
        attempt_cohort_ids(&stored)
            .into_iter()
            .filter(|id| tasks.contains_key(id)),
    );
    let mut task_episodes: HashMap<String, TaskEpisodes> = HashMap::new();
    for id in &episode_ids {
        if let Some(history) = cached_history(runtime, &mut histories, id) {
            task_episodes.insert(id.clone(), TaskEpisodes::read(history));
        }
    }
    // A fix in the lookback can name an origin completed much earlier.
    // Read that origin's durable completion for the detection lag too.
    for task in tasks.values() {
        for link in &task.relations {
            if link.relation_type == TaskRelationType::RegressionFrom
                && !done_at.contains_key(link.target.as_str())
                && let Some(history) = cached_history(runtime, &mut histories, &link.target)
                && let Some(at) = history
                    .iter()
                    .rev()
                    .find(|entry| entry.to_status == Some(TaskStatus::Done))
                    .map(|entry| entry.at)
            {
                done_at.insert(link.target.to_string(), at);
            }
        }
    }
    drop(histories);
    let attempted = task_episodes.iter().any(|(_, episodes)| {
        episodes
            .settled
            .iter()
            .any(|(admitted, _, _)| *admitted >= lookback_start)
    });
    Ok(LoadedForecastHistory {
        tasks,
        stored,
        done_at,
        task_episodes,
        history_rows,
        lookback_start,
        lookback_days,
        min_history,
        arrival_start,
        oldest_forecast,
        return_start,
        attempted,
    })
}
