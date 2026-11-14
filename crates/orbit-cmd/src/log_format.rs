//! Process-log filters, path resolution and plain-text source/code columns
//! shared by `orbit log` and the dashboard's log view.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use orbit_common::protocol::tool_input;
use orbit_core::OrbitError;
use serde_json::Value;

/// Target of the supervisor's per-line agent output events
/// (`orbit-engine`'s `cli_runner/supervisor/output.rs`), the only producer of
/// `provider`/`stream`/`line` fields.
pub const AGENT_OUTPUT_TARGET: &str = "orbit_engine::activity_job::cli_runner::supervisor";

pub fn format_source(target: &str, fields: &Value) -> String {
    // High-value targets get short, fixed labels for the source column.
    if let Some(label) = match target {
        "orbit.policy.deny" => Some("policy"),
        "orbit.friction.reported" => Some("friction"),
        t if t.starts_with("orbit.job.") => Some("job"),
        _ => None,
    } {
        return label.to_string();
    }

    // Agent output events: prefer the `provider` field as the source
    // so the reader sees `claude-4.5` / `codex` / etc. directly.
    if target == AGENT_OUTPUT_TARGET
        && let Some(provider) = fields.get("provider").and_then(Value::as_str)
    {
        return provider.to_string();
    }

    // Generic fallback: the last segment of a `.`- or `::`-qualified target.
    target
        .rsplit([':', '.'])
        .next()
        .unwrap_or(target)
        .to_string()
}

pub fn format_code(target: &str, level: &str, fields: &Value) -> String {
    match target {
        "orbit.policy.deny" => "DENY".to_string(),
        "orbit.friction.reported" => "FRC".to_string(),
        "orbit.job.step_retry" => "RTRY".to_string(),
        "orbit.job.step_finished" => match fields.get("success").and_then(Value::as_bool) {
            Some(true) => "OK".to_string(),
            Some(false) => "ERR".to_string(),
            None => "INF".to_string(),
        },
        _ => match level {
            "ERROR" => "ERR".to_string(),
            "WARN" => "WRN".to_string(),
            "INFO" => "INF".to_string(),
            "DEBUG" => "DBG".to_string(),
            "TRACE" => "TRC".to_string(),
            other => other.chars().take(3).collect::<String>().to_uppercase(),
        },
    }
}

/// Minimum severity accepted by a process-log filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LevelFilter {
    /// Trace records.
    Trace,
    /// Debug records.
    Debug,
    /// Informational records.
    Info,
    /// Warning records.
    Warn,
    /// Error records.
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

    fn from_event_level(level: &str) -> Option<LevelFilter> {
        match level.to_ascii_uppercase().as_str() {
            "TRACE" => Some(LevelFilter::Trace),
            "DEBUG" => Some(LevelFilter::Debug),
            "INFO" => Some(LevelFilter::Info),
            "WARN" => Some(LevelFilter::Warn),
            "ERROR" => Some(LevelFilter::Error),
            _ => None,
        }
    }

    /// Parse a dashboard level, including its warning and error aliases.
    pub fn parse_query(raw: &str) -> Result<Self, OrbitError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "trace" => Ok(LevelFilter::Trace),
            "debug" => Ok(LevelFilter::Debug),
            "info" => Ok(LevelFilter::Info),
            "warn" | "warning" => Ok(LevelFilter::Warn),
            "error" | "err" => Ok(LevelFilter::Error),
            other => Err(OrbitError::InvalidInput(format!(
                "level must be one of trace, debug, info, warn, error; got '{other}'"
            ))),
        }
    }
}

/// Target, severity and inclusive time bounds shared by CLI and web log readers.
#[derive(Clone, Debug, Default)]
pub struct Filters {
    target_prefix: Option<String>,
    min_level: Option<LevelFilter>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
}

impl Filters {
    /// Construct a filter with no upper timestamp bound.
    pub fn new(
        target_prefix: Option<String>,
        min_level: Option<LevelFilter>,
        since: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            target_prefix,
            min_level,
            since,
            until: None,
        }
    }

    /// The configured minimum severity, if any.
    pub fn min_level(&self) -> Option<LevelFilter> {
        self.min_level
    }

    /// The inclusive lower timestamp bound, if any.
    pub fn since(&self) -> Option<DateTime<Utc>> {
        self.since
    }

    /// Exclude records after this inclusive upper bound before counting scan matches.
    pub fn with_until(mut self, until: DateTime<Utc>) -> Self {
        self.until = Some(until);
        self
    }

    /// Judge a JSON log record; missing or unknown severity counts as INFO,
    /// and missing or invalid timestamps do not exclude a record.
    pub fn matches(&self, event: &Value) -> bool {
        self.matches_parts(
            event.get("target").and_then(Value::as_str).unwrap_or(""),
            event.get("level").and_then(Value::as_str),
            event_timestamp(event),
        )
    }

    /// [`Self::matches`] over a record's already extracted fields, so a line
    /// can be judged without building its [`Value`].
    pub fn matches_parts(
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
        if let Some(until) = self.until
            && ts.is_some_and(|ts| ts > until)
        {
            return false;
        }
        true
    }

    /// The instant before which a reverse scan can stop: `since` less
    /// a 60-second timestamp skew allowance. Records are appended in time order,
    /// so the first record this old ends the window; the margin covers records
    /// stamped just before a concurrent writer's earlier stamp reached the file.
    pub fn window_floor(&self) -> Option<DateTime<Utc>> {
        self.since.map(|since| {
            since
                .checked_sub_signed(Duration::seconds(WINDOW_SKEW_SECS))
                .unwrap_or(DateTime::<Utc>::MIN_UTC)
        })
    }

    /// Whether a timestamp is older than the reverse-scan stopping floor.
    pub fn precedes_window(&self, ts: Option<DateTime<Utc>>) -> bool {
        self.window_floor()
            .zip(ts)
            .is_some_and(|(floor, ts)| ts < floor)
    }
}

/// How far out of order the process log's timestamps may run. They are taken
/// when a record is formatted, not when it is appended, and several Orbit
/// processes append to one file, so a record can land after a slightly later one.
const WINDOW_SKEW_SECS: i64 = 60;

/// Extract a parsable RFC 3339 timestamp from a JSON log record.
pub fn event_timestamp(event: &Value) -> Option<DateTime<Utc>> {
    event
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
}

/// Parse a log timestamp as RFC 3339 and normalize it to UTC.
pub fn parse_timestamp(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|ts| ts.with_timezone(&Utc))
}

/// Build CLI log filters using the shared duration/timestamp input grammar.
pub fn build_filters(
    target: Option<String>,
    level: Option<LevelFilter>,
    since: Option<&str>,
) -> Result<Filters, OrbitError> {
    let since = since
        .map(|raw| tool_input::parse_since_with(raw, tool_input::parse_duration_seconds))
        .transpose()?;
    Ok(Filters::new(target, level, since))
}

/// Resolve an explicit log path, then ORBIT_LOG_PATH, then the global JSONL log.
pub fn resolve_log_path(override_path: Option<&Path>) -> Result<PathBuf, OrbitError> {
    if let Some(path) = override_path {
        return Ok(path.to_path_buf());
    }
    if let Ok(env) = std::env::var("ORBIT_LOG_PATH")
        && !env.is_empty()
    {
        return Ok(PathBuf::from(env));
    }
    orbit_common::observability::logging::global_jsonl_log_path().map_err(|err| {
        OrbitError::InvalidInput(format!("cannot resolve global JSONL log path: {err}"))
    })
}
