//! Level, target and time-window filters.

use crate::parse::parse_since;
use chrono::{DateTime, Duration, Utc};
use clap::ValueEnum;
use orbit_core::OrbitError;
use serde_json::Value;

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq, PartialOrd, Ord)]
#[clap(rename_all = "lower")]
pub(crate) enum LevelFilter {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LevelFilter {
    fn rank(self) -> u8 {
        match self {
            LevelFilter::Trace => 0,
            LevelFilter::Debug => 1,
            LevelFilter::Info => 2,
            LevelFilter::Warn => 3,
            LevelFilter::Error => 4,
        }
    }

    pub(crate) fn from_event_level(level: &str) -> Option<LevelFilter> {
        match level.to_ascii_uppercase().as_str() {
            "TRACE" => Some(LevelFilter::Trace),
            "DEBUG" => Some(LevelFilter::Debug),
            "INFO" => Some(LevelFilter::Info),
            "WARN" => Some(LevelFilter::Warn),
            "ERROR" => Some(LevelFilter::Error),
            _ => None,
        }
    }

    pub(crate) fn parse_query(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "trace" => Ok(LevelFilter::Trace),
            "debug" => Ok(LevelFilter::Debug),
            "info" => Ok(LevelFilter::Info),
            "warn" | "warning" => Ok(LevelFilter::Warn),
            "error" | "err" => Ok(LevelFilter::Error),
            other => Err(format!(
                "level must be one of trace, debug, info, warn, error; got '{other}'"
            )),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Filters {
    target_prefix: Option<String>,
    pub(super) min_level: Option<LevelFilter>,
    pub(super) since: Option<DateTime<Utc>>,
}

impl Filters {
    pub(crate) fn new(
        target_prefix: Option<String>,
        min_level: Option<LevelFilter>,
        since: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            target_prefix,
            min_level,
            since,
        }
    }

    pub(crate) fn from_query_parts(
        target: Option<String>,
        level: Option<String>,
        since: Option<&str>,
    ) -> Result<Self, OrbitError> {
        let min_level = match level.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => Some(LevelFilter::parse_query(raw).map_err(OrbitError::InvalidInput)?),
            None => None,
        };
        let since = since.map(parse_since).transpose()?;
        Ok(Self::new(target, min_level, since))
    }

    pub(crate) fn matches(&self, event: &Value) -> bool {
        self.matches_parts(
            event.get("target").and_then(Value::as_str).unwrap_or(""),
            event.get("level").and_then(Value::as_str),
            event_timestamp(event),
        )
    }

    /// [`Self::matches`] over a record's already extracted fields, so a line
    /// can be judged without building its [`Value`].
    pub(super) fn matches_parts(
        &self,
        target: &str,
        level: Option<&str>,
        ts: Option<DateTime<Utc>>,
    ) -> bool {
        if let Some(prefix) = &self.target_prefix
            && !target.starts_with(prefix)
        {
            return false;
        }
        if let Some(min) = self.min_level {
            let event_level =
                LevelFilter::from_event_level(level.unwrap_or("INFO")).unwrap_or(LevelFilter::Info);
            if event_level.rank() < min.rank() {
                return false;
            }
        }
        if let Some(since) = self.since
            && ts.is_some_and(|ts| ts < since)
        {
            return false;
        }
        true
    }

    /// The instant before which a reverse scan can stop: `since` less
    /// [`WINDOW_SKEW`]. Records are appended in time order, so the first record
    /// this old ends the window; the margin covers records stamped just before
    /// a concurrent writer's earlier stamp reached the file.
    pub(super) fn window_floor(&self) -> Option<DateTime<Utc>> {
        self.since.map(|since| {
            since
                .checked_sub_signed(Duration::seconds(WINDOW_SKEW_SECS))
                .unwrap_or(DateTime::<Utc>::MIN_UTC)
        })
    }

    pub(super) fn precedes_window(&self, ts: Option<DateTime<Utc>>) -> bool {
        self.window_floor()
            .zip(ts)
            .is_some_and(|(floor, ts)| ts < floor)
    }
}

/// How far out of order the process log's timestamps may run. They are taken
/// when a record is formatted, not when it is appended, and several Orbit
/// processes append to one file, so a record can land after a slightly later one.
const WINDOW_SKEW_SECS: i64 = 60;

pub(super) fn event_timestamp(event: &Value) -> Option<DateTime<Utc>> {
    event
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
}

pub(super) fn parse_timestamp(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|ts| ts.with_timezone(&Utc))
}
