use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{TaskStatus, complexity_bucket};

use crate::OrbitRuntime;

use super::history::LoadedForecastHistory;
use super::model::Delivery;
use super::outcomes::{AwayWait, Episode, EpisodeOutcome, RETURN_HORIZON_HOURS};
use super::review_modes::ReviewHistory;
use super::usage::ForecastRun;
use super::{outcomes, usage};

pub(super) struct ForecastCohorts {
    pub(super) runs: HashMap<String, Vec<ForecastRun>>,
    pub(super) reviews: ReviewHistory,
    pub(super) history: Vec<Delivery>,
    pub(super) without_invocations: u64,
    pub(super) episodes: Vec<Episode>,
    pub(super) waits: Vec<AwayWait>,
    pub(super) live_episodes: u64,
}
/// How far before the oldest task or forecast the invocation read starts.
const SPEND_READ_SLACK: Duration = Duration::days(1);

pub(super) fn prepare_forecast_cohorts(
    runtime: &OrbitRuntime,
    loaded: &LoadedForecastHistory,
    now: DateTime<Utc>,
) -> Result<ForecastCohorts, OrbitError> {
    let tasks = &loaded.tasks;
    let done_at = &loaded.done_at;
    let task_episodes = &loaded.task_episodes;
    let history_rows = &loaded.history_rows;
    let lookback_start = loaded.lookback_start;
    let oldest_forecast = loaded.oldest_forecast;
    let return_start = loaded.return_start;
    let attempted = loaded.attempted;
    // The invocation read starts at the earliest of the lookback, the oldest
    // task it must cover and the oldest stored forecast. Creation times
    // alone are not relied on as the bound, since nothing guarantees a task
    // record's creation time precedes the runs linked to it; the lookback
    // start costs little, as the oldest history task usually predates it.
    let in_progress_created = tasks
        .values()
        .filter(|task| task.status == TaskStatus::InProgress)
        .map(|task| task.created_at);
    let spend_since = history_rows
        .iter()
        .filter_map(|row| row.created_at)
        .chain(in_progress_created)
        .chain(oldest_forecast)
        .chain((!history_rows.is_empty() || attempted).then_some(lookback_start))
        .min()
        .map(|oldest| oldest - SPEND_READ_SLACK);
    let mut runs: HashMap<String, Vec<ForecastRun>> = HashMap::new();
    let spends = if spend_since.is_some() {
        runtime.invocation_spend(spend_since)?
    } else {
        Vec::new()
    };
    let review_task_ids = history_rows
        .iter()
        .map(|row| row.task_id.as_str())
        .chain(task_episodes.keys().map(String::as_str))
        .collect();
    let reviews = ReviewHistory::collect(runtime, &spends, &review_task_ids)?;
    if spend_since.is_some() {
        for spend in spends {
            let end = spend.ts;
            let start = end - Duration::milliseconds(i64::try_from(spend.duration_ms).unwrap_or(0));
            for task_id in &spend.task_ids {
                runs.entry(task_id.clone()).or_default().push(ForecastRun {
                    start,
                    end,
                    tokens: spend.normalized_tokens.map(|tokens| tokens as f64),
                    usd: spend.provider_cost_usd,
                });
            }
        }
        for task_runs in runs.values_mut() {
            task_runs.sort_by_key(|run| run.start);
        }
    }
    let no_runs: &[ForecastRun] = &[];

    let mut history = Vec::new();
    let mut without_invocations = 0u64;
    let task_runs = |id: &str| runs.get(id).map_or(no_runs, Vec::as_slice);
    let last_setback = |id: &str, until: Option<DateTime<Utc>>| {
        task_episodes
            .get(id)
            .and_then(|episodes| episodes.last_setback(until))
    };
    for row in history_rows {
        // Attempts that did not deliver are sampled as episodes, so the
        // delivery counts only what followed the last of them.
        let since = last_setback(&row.task_id, done_at.get(&row.task_id).copied());
        let used = usage::forecast_usage(task_runs(&row.task_id), since);
        if used.invocations == 0 {
            without_invocations += 1;
            continue;
        }
        history.push(Delivery {
            task_id: row.task_id.clone(),
            complexity: row.complexity.clone(),
            crew: tasks.get(&row.task_id).and_then(|task| task.crew.clone()),
            review_mode: reviews.delivery_mode(&row.task_id),
            hours: used.hours,
            tokens: used.tokens,
            usd: used.usd,
        });
    }

    let sample_cell = |id: &str| {
        let task = tasks.get(id);
        (
            complexity_bucket(task.and_then(|task| task.complexity.map(|c| c.as_str())))
                .to_string(),
            task.and_then(|task| task.crew.clone()),
            reviews.delivery_mode(id),
        )
    };
    let mut episodes: Vec<Episode> = Vec::new();
    let mut waits: Vec<AwayWait> = Vec::new();
    let mut live_episodes = 0u64;
    let mut episode_ids: Vec<&String> = task_episodes.keys().collect();
    episode_ids.sort();
    for id in episode_ids {
        let recorded = &task_episodes[id];
        let (complexity, crew, review_mode) = sample_cell(id);
        if recorded
            .live
            .is_some_and(|admitted| admitted >= lookback_start)
        {
            live_episodes += 1;
        }
        for (admitted, settled, outcome) in &recorded.settled {
            if *admitted < lookback_start {
                continue;
            }
            let (tokens, usd) = if *outcome == EpisodeOutcome::Delivered {
                (None, None)
            } else {
                usage::window_spend(task_runs(id), *admitted, *settled)
            };
            episodes.push(Episode {
                complexity: complexity.clone(),
                crew: crew.clone(),
                review_mode,
                outcome: *outcome,
                hold_hours: outcomes::hours_between(*admitted, *settled),
                tokens,
                usd,
            });
        }
        for (left, follow) in &recorded.away {
            if *left < return_start {
                continue;
            }
            let (hours, event) = match follow {
                Some((at, event)) => (outcomes::hours_between(*left, *at), Some(*event)),
                None => (outcomes::hours_between(*left, now), None),
            };
            // Past the horizon a wait is censored there: no return within it.
            let (hours, event) = if hours > RETURN_HORIZON_HOURS {
                (RETURN_HORIZON_HOURS, None)
            } else {
                (hours, event)
            };
            waits.push(AwayWait {
                complexity: complexity.clone(),
                crew: crew.clone(),
                review_mode,
                hours,
                event,
            });
        }
    }

    Ok(ForecastCohorts {
        runs,
        reviews,
        history,
        without_invocations,
        episodes,
        waits,
        live_episodes,
    })
}
