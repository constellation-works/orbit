use orbit_common::OrbitError;

/// Default history lookback, in days.
pub(super) const DEFAULT_LOOKBACK_DAYS: i64 = 30;
const MIN_LOOKBACK_DAYS: i64 = 7;
const MAX_LOOKBACK_DAYS: i64 = 180;
/// The default lookback reaches back past its days to at least this many
/// completed deliveries.
pub(super) const MIN_HISTORY_TASKS: usize = 50;
/// The most completed deliveries `history_tasks` may select.
const MAX_HISTORY_TASKS: usize = 5_000;
/// The most leaf runs a request may simulate.
const MAX_CONCURRENCY: u32 = 64;
pub(super) struct Options {
    pub(super) lookback_days: i64,
    /// History is the newest this many completions, whatever their age,
    /// instead of `lookback_days`.
    pub(super) history_tasks: Option<usize>,
    pub(super) concurrency: Option<u32>,
    pub(super) arrivals: bool,
}

impl Options {
    /// Only the canonical forecast is stored, so calibration compares like
    /// with like.
    pub(super) fn canonical(&self) -> bool {
        self.lookback_days == DEFAULT_LOOKBACK_DAYS
            && self.history_tasks.is_none()
            && self.concurrency.is_none()
            && !self.arrivals
    }
}

/// How far back delivery history reaches. The default is the canonical
/// lookback.
#[derive(Debug, Clone, Copy, Default)]
pub struct BacklogForecastHistory {
    /// Completions in the last this many days (7–180, default 30), and at
    /// least the newest 50.
    pub lookback_days: Option<i64>,
    /// Only the newest this many completions (1–5000), whatever their age.
    pub history_tasks: Option<usize>,
}

pub(super) fn parse_options(
    history: BacklogForecastHistory,
    concurrency: Option<u32>,
    arrivals: Option<&str>,
) -> Result<Options, OrbitError> {
    let invalid = |message: String| OrbitError::InvalidInput(message);
    if history.lookback_days.is_some() && history.history_tasks.is_some() {
        return Err(invalid(
            "lookback_days and history_tasks are alternatives: give one".to_string(),
        ));
    }
    if let Some(tasks) = history.history_tasks
        && !(1..=MAX_HISTORY_TASKS).contains(&tasks)
    {
        return Err(invalid(format!(
            "history_tasks must be between 1 and {MAX_HISTORY_TASKS}"
        )));
    }
    let lookback_days = history.lookback_days.unwrap_or(DEFAULT_LOOKBACK_DAYS);
    if !(MIN_LOOKBACK_DAYS..=MAX_LOOKBACK_DAYS).contains(&lookback_days) {
        return Err(invalid(format!(
            "lookback_days must be between {MIN_LOOKBACK_DAYS} and {MAX_LOOKBACK_DAYS}"
        )));
    }
    if let Some(concurrency) = concurrency
        && !(1..=MAX_CONCURRENCY).contains(&concurrency)
    {
        return Err(invalid(format!(
            "concurrency must be between 1 and {MAX_CONCURRENCY}"
        )));
    }
    let arrivals = match arrivals.unwrap_or("off") {
        "off" => false,
        "on" => true,
        other => {
            return Err(invalid(format!(
                "unknown arrivals '{other}': expected on or off"
            )));
        }
    };
    Ok(Options {
        lookback_days,
        history_tasks: history.history_tasks,
        concurrency,
        arrivals,
    })
}
