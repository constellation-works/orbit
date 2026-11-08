use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::facts::SCAN_DAYS;
use super::{
    AgentFacts, AgentQuery, COUNT_WINDOWS, DEFAULT_MIN_SUPPORT, DEFAULT_WINDOW, ExecutionScope,
    WINDOWS, WindowSpan,
};

impl AgentQuery {
    /// Validate a selection. An omitted window is `30d` and an omitted
    /// minimum support 10; implementer metrics count managed tasks until
    /// [`Self::with_execution`] says otherwise.
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
            execution: ExecutionScope::Managed,
            until: None,
        })
    }

    /// Narrow implementer metrics to `managed` (the default when omitted),
    /// `all` or `interactive` tasks.
    pub fn with_execution(mut self, execution: Option<&str>) -> Result<Self, OrbitError> {
        let Some(name) = execution.map(str::trim).filter(|name| !name.is_empty()) else {
            return Ok(self);
        };
        self.execution = ExecutionScope::ALL
            .into_iter()
            .find(|scope| scope.as_str() == name)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "execution must be one of managed, all or interactive; got '{name}'"
                ))
            })?;
        Ok(self)
    }

    /// Which executions implementer metrics count.
    pub fn execution(&self) -> ExecutionScope {
        self.execution
    }

    /// Set the exclusive window end. `None` leaves it open. A blank string
    /// is refused. An RFC 3339 instant, or a duration ago (`30d`, and the
    /// `m`/`h`/`d`/`w` units a resident metrics window accepts), resolves to
    /// an absolute instant. A future instant is refused.
    pub fn with_until(mut self, until: Option<&str>) -> Result<Self, OrbitError> {
        let Some(raw) = until else {
            return Ok(self);
        };
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(until_rejected(raw));
        }
        self.until = Some(parse_until(raw, Utc::now())?);
        Ok(self)
    }

    /// The served bounds. A time window is `[until - span, until)` when
    /// `until` is set, and `[now - span, ...)` with no upper filter when it
    /// is not. A count window takes the newest `n` attempt starts before
    /// `until` (or in the scan, when `until` is omitted). `since` is clamped
    /// so it is not after a set `until`.
    pub(super) fn bounds(&self, facts: &AgentFacts) -> WindowBounds {
        let now = Utc::now();
        let reported_until = self.until.unwrap_or(now);
        let mut since = match self.span {
            WindowSpan::Hours(hours) => reported_until
                .checked_sub_signed(chrono::Duration::hours(hours))
                .unwrap_or(reported_until),
            WindowSpan::Attempts(count) => {
                let horizon = facts.computed_at - chrono::Duration::days(SCAN_DAYS);
                let mut starts: Vec<DateTime<Utc>> = facts
                    .tasks
                    .iter()
                    .flat_map(|task| task.attempts.iter().map(|attempt| attempt.start))
                    .filter(|start| *start >= horizon && self.until.is_none_or(|end| *start < end))
                    .collect();
                starts.sort_unstable_by(|a, b| b.cmp(a));
                starts
                    .get(count.min(starts.len()).saturating_sub(1))
                    .copied()
                    .unwrap_or(horizon)
            }
        };
        if let Some(end) = self.until {
            since = since.min(end);
        }
        WindowBounds {
            since,
            until_exclusive: self.until,
            reported_until,
        }
    }

    /// The selection and scan facts every response carries.
    pub(super) fn header(
        &self,
        facts: &AgentFacts,
        bounds: WindowBounds,
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
            "since": bounds.since().to_rfc3339_opts(SecondsFormat::Secs, true),
            "until": bounds.reported_until().to_rfc3339_opts(SecondsFormat::Secs, true),
            "min_support": self.min_support,
            // Which derived executions implementer metrics count.
            "execution": self.execution.as_str(),
            "computed_at": facts.computed_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            "cache_ttl_seconds": cache_ttl.as_secs(),
            "tasks_scanned": facts.tasks_scanned,
        })
    }
}

/// The served window: inclusive `since`, optional exclusive `until`, and the
/// `until` instant the payload reports. An omitted `until` leaves the upper
/// bound open and reports the instant used as now.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WindowBounds {
    since: DateTime<Utc>,
    until_exclusive: Option<DateTime<Utc>>,
    reported_until: DateTime<Utc>,
}

impl WindowBounds {
    /// Whether `at` falls in the served window.
    pub(crate) fn admits(self, at: DateTime<Utc>) -> bool {
        at >= self.since && self.until_exclusive.is_none_or(|end| at < end)
    }

    /// The served inclusive start, after clamping it to `until` when set.
    pub(crate) fn since(self) -> DateTime<Utc> {
        self.since
    }

    /// The served end the payload reports. When `until` was omitted this is
    /// the instant used as now, and [`Self::admits`] does not filter on it.
    pub(crate) fn reported_until(self) -> DateTime<Utc> {
        self.reported_until
    }
}

fn until_rejected(raw: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "until must be an RFC 3339 instant or a duration ago such as 30d; got '{raw}'"
    ))
}

/// An RFC 3339 instant, or a duration before `now`. Units match the resident
/// metrics window: `m`/`min`/`minute`, `h`/`hour`, `d`/`day`, `w`/`week`,
/// with whitespace ignored and a trailing `s` trimmed. A bare date is not
/// accepted. `until` in the future is refused.
pub(super) fn parse_until(raw: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, OrbitError> {
    if let Ok(at) = DateTime::parse_from_rfc3339(raw) {
        let at = at.with_timezone(&Utc);
        if at > now {
            return Err(OrbitError::InvalidInput(
                "until must not be in the future".to_string(),
            ));
        }
        return Ok(at);
    }
    let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    let compact = compact.trim_end_matches('s');
    let Some(split) = compact.find(|c: char| c.is_ascii_alphabetic()) else {
        return Err(until_rejected(raw));
    };
    let (amount, unit) = compact.split_at(split);
    let Ok(amount) = amount.parse::<i64>() else {
        return Err(until_rejected(raw));
    };
    if amount < 0 {
        return Err(until_rejected(raw));
    }
    let span = match unit {
        "m" | "min" | "minute" => chrono::Duration::try_minutes(amount),
        "h" | "hour" => chrono::Duration::try_hours(amount),
        "d" | "day" => chrono::Duration::try_days(amount),
        "w" | "week" => chrono::Duration::try_weeks(amount),
        _ => return Err(until_rejected(raw)),
    };
    span.and_then(|span| now.checked_sub_signed(span))
        .ok_or_else(|| OrbitError::InvalidInput(format!("until is out of range: {raw}")))
}
