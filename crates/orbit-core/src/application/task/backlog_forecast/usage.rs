use chrono::{DateTime, Duration, Utc};

/// Gaps between a task's invocations shorter than this count as delivery
/// time (CI, review and gate waits inside a run); longer ones (the task
/// waiting in the backlog between attempts) do not.
pub(super) const GAP_BRIDGE_HOURS: f64 = 2.0;
/// One managed invocation linked to a task: when it ran and what it used.
pub(in crate::application::task) struct ForecastRun {
    pub(super) start: DateTime<Utc>,
    pub(super) end: DateTime<Utc>,
    pub(super) tokens: Option<f64>,
    pub(super) usd: Option<f64>,
}

/// What a task's invocations used, from some time on.
pub(in crate::application::task) struct ForecastUsage {
    pub(in crate::application::task) invocations: usize,
    pub(in crate::application::task) hours: f64,
    /// `None` when an invocation ran an uncovered model.
    pub(in crate::application::task) tokens: Option<f64>,
    /// `None` when an invocation recorded no valid provider cost.
    pub(in crate::application::task) usd: Option<f64>,
    pub(in crate::application::task) last_end: Option<DateTime<Utc>>,
}

/// The usage of `runs` (sorted by start) that ended after `after`; a run
/// that started before `after` counts from it.
pub(in crate::application::task) fn forecast_usage(
    runs: &[ForecastRun],
    after: Option<DateTime<Utc>>,
) -> ForecastUsage {
    let bridge = Duration::seconds((GAP_BRIDGE_HOURS * 3600.0) as i64);
    let mut used = ForecastUsage {
        invocations: 0,
        hours: 0.0,
        tokens: Some(0.0),
        usd: Some(0.0),
        last_end: None,
    };
    let mut open: Option<(DateTime<Utc>, DateTime<Utc>)> = None;
    let mut busy = Duration::zero();
    for run in runs {
        if after.is_some_and(|after| run.end <= after) {
            continue;
        }
        let start = after.map_or(run.start, |after| run.start.max(after));
        used.invocations += 1;
        used.tokens = used.tokens.zip(run.tokens).map(|(sum, one)| sum + one);
        used.usd = used.usd.zip(run.usd).map(|(sum, one)| sum + one);
        used.last_end = used.last_end.max(Some(run.end));
        open = match open {
            Some((from, to)) if start <= to + bridge => Some((from, to.max(run.end))),
            Some((from, to)) => {
                busy += to - from;
                Some((start, run.end))
            }
            None => Some((start, run.end)),
        };
    }
    if let Some((from, to)) = open {
        busy += to - from;
    }
    used.hours = busy.num_seconds().max(0) as f64 / 3600.0;
    used
}

/// What the runs overlapping `from`..`to` used: one attempt's tokens and USD.
pub(super) fn window_spend(
    runs: &[ForecastRun],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> (Option<f64>, Option<f64>) {
    runs.iter()
        .filter(|run| run.end > from && run.start < to)
        .fold((Some(0.0), Some(0.0)), |(tokens, usd), run| {
            (
                tokens.zip(run.tokens).map(|(sum, one)| sum + one),
                usd.zip(run.usd).map(|(sum, one)| sum + one),
            )
        })
}
