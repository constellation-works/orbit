use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::facts::SCAN_DAYS;
use super::{
    AgentFacts, AgentQuery, COUNT_WINDOWS, DEFAULT_MIN_SUPPORT, DEFAULT_WINDOW, WINDOWS, WindowSpan,
};

impl AgentQuery {
    /// Validate a selection. An omitted window is `30d` and an omitted
    /// minimum support 10.
    pub fn parse(window: Option<&str>, min_support: Option<usize>) -> Result<Self, OrbitError> {
        let window = window.unwrap_or(DEFAULT_WINDOW);
        let timed = WINDOWS
            .iter()
            .find(|(name, _, _)| *name == window)
            .map(|&(requested, served, hours)| (requested, served, WindowSpan::Hours(hours)));
        let counted = || {
            COUNT_WINDOWS
                .iter()
                .find(|(name, _)| *name == window)
                .map(|&(name, count)| (name, name, WindowSpan::Attempts(count)))
        };
        let Some((requested, served, span)) = timed.or_else(counted) else {
            return Err(OrbitError::InvalidInput(format!(
                "window must be one of 1h, 24h, 7d, 30d, 90d, all, last50, last200, last500 or last1000; got '{window}'"
            )));
        };
        let min_support = min_support.unwrap_or(DEFAULT_MIN_SUPPORT);
        if min_support == 0 {
            return Err(OrbitError::InvalidInput(
                "min_support must be at least 1".to_string(),
            ));
        }
        Ok(Self {
            requested,
            window: served,
            span,
            min_support,
        })
    }

    /// The window's start; it ends now. A count window starts at the newest
    /// `n`th delivery attempt in the scan, or its oldest when the scan holds
    /// fewer, and at the scan's reach when it holds none.
    pub(super) fn since(&self, facts: &AgentFacts) -> DateTime<Utc> {
        match self.span {
            WindowSpan::Hours(hours) => Utc::now() - chrono::Duration::hours(hours),
            WindowSpan::Attempts(count) => {
                let horizon = facts.computed_at - chrono::Duration::days(SCAN_DAYS);
                let mut starts: Vec<DateTime<Utc>> = facts
                    .tasks
                    .iter()
                    .flat_map(|task| task.attempts.iter().map(|attempt| attempt.start))
                    .filter(|start| *start >= horizon)
                    .collect();
                starts.sort_unstable_by(|a, b| b.cmp(a));
                starts
                    .get(count.min(starts.len()).saturating_sub(1))
                    .copied()
                    .unwrap_or(horizon)
            }
        }
    }

    /// A time window's start, for reports that read no scan. A count window
    /// is refused: the delivery attempts it counts come from the scan.
    pub(super) fn time_since(&self) -> Result<DateTime<Utc>, OrbitError> {
        match self.span {
            WindowSpan::Hours(hours) => Ok(Utc::now() - chrono::Duration::hours(hours)),
            WindowSpan::Attempts(_) => Err(OrbitError::InvalidInput(format!(
                "window {} counts delivery attempts, which this report does not read: use 1h, 24h, 7d, 30d, 90d or all",
                self.requested
            ))),
        }
    }

    /// The selection and scan facts every response carries.
    pub(super) fn header(
        &self,
        facts: &AgentFacts,
        since: DateTime<Utc>,
        cache_ttl: Duration,
    ) -> Value {
        json!({
            "window": self.window,
            "requested_window": self.requested,
            // A count window's size in delivery attempts; `null` for time.
            "window_attempts": match self.span {
                WindowSpan::Hours(_) => None,
                WindowSpan::Attempts(count) => Some(count),
            },
            "since": since.to_rfc3339_opts(SecondsFormat::Secs, true),
            "min_support": self.min_support,
            "computed_at": facts.computed_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            "cache_ttl_seconds": cache_ttl.as_secs(),
            "tasks_scanned": facts.tasks_scanned,
        })
    }
}
