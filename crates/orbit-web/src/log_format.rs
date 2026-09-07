//! Web-focused subset of log tailing / rendering logic (no colored output, no
//! clap ValueEnum usage for CLI flags). Preserves exact `resolve_log_path`
//! (ORBIT_LOG_PATH + HOME fallback via orbit-common) and the HTML rendering
//! used by /api/log and /api/diagnostics.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use clap::ValueEnum;
use orbit_core::OrbitError;
use serde::Serialize;
use serde_json::Value;

use crate::parse::parse_since;

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
    min_level: Option<LevelFilter>,
    since: Option<DateTime<Utc>>,
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
        let target = event.get("target").and_then(Value::as_str).unwrap_or("");
        if let Some(prefix) = &self.target_prefix
            && !target.starts_with(prefix)
        {
            return false;
        }
        if let Some(min) = self.min_level {
            let level = event.get("level").and_then(Value::as_str).unwrap_or("INFO");
            let event_level = LevelFilter::from_event_level(level).unwrap_or(LevelFilter::Info);
            if event_level.rank() < min.rank() {
                return false;
            }
        }
        if let Some(since) = self.since
            && let Some(ts) = event.get("timestamp").and_then(Value::as_str)
            && let Ok(parsed) = DateTime::parse_from_rfc3339(ts)
            && parsed.with_timezone(&Utc) < since
        {
            return false;
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RenderedLogEvent {
    pub ts: String,
    pub source: String,
    pub code: String,
    pub level: String,
    pub message_html: String,
}

pub(crate) fn resolve_log_path(override_path: Option<&Path>) -> Result<PathBuf, OrbitError> {
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

pub(crate) fn read_recent_matching_events(
    path: &Path,
    filters: &Filters,
    limit: usize,
) -> io::Result<Vec<Value>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let reader = BufReader::new(file);
    let mut kept = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if let Some(event) = parse_matching_event(&line, filters) {
            kept.push(event);
            if kept.len() > limit {
                kept.remove(0);
            }
        }
    }
    Ok(kept)
}

pub(crate) fn read_recent_rendered_events(
    path: &Path,
    filters: &Filters,
    limit: usize,
) -> io::Result<Vec<RenderedLogEvent>> {
    Ok(read_recent_matching_events(path, filters, limit)?
        .into_iter()
        .map(|event| render_log_event_for_web(&event))
        .collect())
}

pub(crate) fn parse_matching_event(raw: &str, filters: &Filters) -> Option<Value> {
    let value = serde_json::from_str::<Value>(raw).ok()?;
    filters.matches(&value).then_some(value)
}

pub(crate) fn render_log_event_for_web(event: &Value) -> RenderedLogEvent {
    let ts = event
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let level_raw = event.get("level").and_then(Value::as_str).unwrap_or("INFO");
    let target = event.get("target").and_then(Value::as_str).unwrap_or("-");
    let fields = event
        .get("fields")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));

    RenderedLogEvent {
        ts,
        source: format_source(target, &fields),
        code: format_code(target, level_raw, &fields),
        level: normalize_level(level_raw).to_string(),
        message_html: format_message_html(target, &fields),
    }
}

fn normalize_level(level: &str) -> &'static str {
    match level.to_ascii_uppercase().as_str() {
        "TRACE" => "trace",
        "DEBUG" => "debug",
        "WARN" => "warn",
        "ERROR" => "error",
        _ => "info",
    }
}

pub(crate) fn format_source(target: &str, fields: &Value) -> String {
    if let Some(label) = match target {
        "orbit.policy.deny" => Some("policy"),
        "orbit.friction.reported" => Some("friction"),
        t if t.starts_with("orbit.job.") => Some("job"),
        _ => None,
    } {
        return label.to_string();
    }

    if target == "orbit_engine::activity_job::cli_runner"
        && let Some(provider) = fields.get("provider").and_then(Value::as_str)
    {
        return provider.to_string();
    }

    target
        .rsplit_once('.')
        .map(|(_, tail)| tail.to_string())
        .unwrap_or_else(|| target.to_string())
}

pub(crate) fn format_code(target: &str, level: &str, fields: &Value) -> String {
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

pub(crate) fn format_message_html(target: &str, fields: &Value) -> String {
    let getf = |k: &str| fields.get(k).and_then(Value::as_str).unwrap_or("");
    let getn = |k: &str| -> String {
        fields
            .get(k)
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default()
    };

    match target {
        "orbit.policy.deny" => html_pairs(&[
            ("tool", getf("tool").to_string()),
            ("path", getf("path").to_string()),
            ("profile", getf("profile").to_string()),
            ("rule", getf("matched_rule").to_string()),
        ]),
        "orbit.friction.reported" => {
            let mut s = format!(
                "friction reported on {}",
                code_value(getf("task_id").to_string())
            );
            let agent = getf("agent");
            let model = getf("model");
            if !agent.is_empty() || !model.is_empty() {
                s.push_str(" by ");
                s.push_str(&code_value(format!("{agent}/{model}")));
            }
            let summary = getf("summary");
            if !summary.is_empty() {
                s.push_str(": ");
                s.push_str(&escape_html(summary));
            }
            s
        }
        "orbit.job.step_started" => format!(
            "step {} started [run={}]",
            code_value(getf("step_id").to_string()),
            code_value(getf("job_run_id").to_string()),
        ),
        "orbit.job.step_finished" => {
            let step = code_value(getf("step_id").to_string());
            let outcome = code_value(getf("outcome").to_string());
            match fields.get("success").and_then(Value::as_bool) {
                Some(true) => format!("step {step} finished ok ({outcome})"),
                Some(false) | None => format!("step {step} finished {outcome}"),
            }
        }
        "orbit.job.step_retry" => format!(
            "step {} retry attempt={} backoff_ms={}",
            code_value(getf("step_id").to_string()),
            code_value(getn("attempt")),
            code_value(getn("next_backoff_ms")),
        ),
        "orbit.job.step_skipped" => {
            format!(
                "step {} skipped: {}",
                code_value(getf("step_id").to_string()),
                escape_html(getf("reason")),
            )
        }
        "orbit.job.step_denied" => {
            format!(
                "step {} denied: {}",
                code_value(getf("step_id").to_string()),
                escape_html(getf("reason")),
            )
        }
        "orbit.job.fanout" => html_pairs(&[
            ("phase", getf("phase").to_string()),
            ("step", getf("step_id").to_string()),
            ("workers", getn("worker_count")),
            ("collected", getn("collected")),
            ("failed", getn("failed")),
        ]),
        "orbit.job.worker_state" => format!(
            "worker[{}] state={} step={}",
            code_value(getn("worker_index")),
            code_value(getf("state").to_string()),
            code_value(getf("step_id").to_string()),
        ),
        "orbit.job.loop_iteration" => format!(
            "loop {} phase={} step={}",
            code_value(getn("iteration")),
            code_value(getf("phase").to_string()),
            code_value(getf("step_id").to_string()),
        ),
        "orbit.job.loop_did_not_converge" => format!(
            "loop step={} did not converge after {} iterations",
            code_value(getf("step_id").to_string()),
            code_value(getn("max_iterations")),
        ),
        "orbit_engine::activity_job::cli_runner" => {
            let stream = getf("stream");
            let line = getf("line");
            if !stream.is_empty() {
                format!("[{}] {}", code_value(stream.to_string()), escape_html(line))
            } else {
                escape_html(line)
            }
        }
        _ => {
            let mut parts: Vec<String> = Vec::new();
            if let Value::Object(map) = fields {
                if let Some(message) = map.get("message").and_then(Value::as_str) {
                    parts.push(escape_html(message));
                }
                for (k, v) in map {
                    if k == "message" {
                        continue;
                    }
                    let value_str = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    parts.push(format!(
                        "<b>{}</b>={}",
                        escape_html(k),
                        code_value(value_str)
                    ));
                }
            }
            parts.join(" ")
        }
    }
}

fn html_pairs(pairs: &[(&str, String)]) -> String {
    pairs
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| format!("<b>{}</b>={}", escape_html(key), code_value(value.clone())))
        .collect::<Vec<_>>()
        .join(" ")
}

fn code_value(value: String) -> String {
    format!("<code>{}</code>", escape_html(&value))
}

fn escape_html(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}
