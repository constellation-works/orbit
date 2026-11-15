use chrono::{DateTime, Utc};
use clap::ValueEnum;
use colored::{ColoredString, Colorize};
use orbit_cmd::log_format::{self, AGENT_OUTPUT_TARGET};
use serde_json::Value;

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq, PartialOrd, Ord)]
#[clap(rename_all = "lower")]
pub enum LevelFilter {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl From<LevelFilter> for log_format::LevelFilter {
    fn from(level: LevelFilter) -> Self {
        match level {
            LevelFilter::Trace => Self::Trace,
            LevelFilter::Debug => Self::Debug,
            LevelFilter::Info => Self::Info,
            LevelFilter::Warn => Self::Warn,
            LevelFilter::Error => Self::Error,
        }
    }
}

pub(crate) fn format_event_line(event: &Value, use_color: bool) -> String {
    let timestamp = event
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or("--:--:--");
    let level = event.get("level").and_then(Value::as_str).unwrap_or("INFO");
    let target = event.get("target").and_then(Value::as_str).unwrap_or("-");
    let fields = event
        .get("fields")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));

    let time_col = format_timestamp(timestamp);
    let source_col = log_format::format_source(target, &fields);
    let code_col = log_format::format_code(target, level, &fields);
    let message_col = format_message(target, &fields);

    if use_color {
        format!(
            "{time}  {source:14}  {code}  {message}",
            time = time_col.dimmed(),
            source = colorize_source(target, &source_col),
            code = colorize_code(target, level, &code_col),
            message = message_col,
        )
    } else {
        format!("{time_col}  {source_col:14}  {code_col:5}  {message_col}")
    }
}

fn format_timestamp(raw: &str) -> String {
    // Accept ISO-8601 (RFC3339); display as HH:MM:SS in local-ish UTC. If the
    // string doesn't parse, render its first 8 chars after stripping the date.
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw) {
        return parsed.with_timezone(&Utc).format("%H:%M:%S").to_string();
    }
    if let Some(idx) = raw.find('T') {
        let after_t = &raw[idx + 1..];
        return after_t.chars().take(8).collect();
    }
    raw.chars().take(8).collect()
}

fn format_message(target: &str, fields: &Value) -> String {
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
        "orbit.policy.deny" => {
            let tool = getf("tool");
            let path = getf("path");
            let profile = getf("profile");
            let rule = getf("matched_rule");
            let mut s = String::new();
            if !tool.is_empty() {
                s.push_str(&format!("tool={tool}"));
            }
            if !path.is_empty() {
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push_str(&format!("path={path}"));
            }
            if !profile.is_empty() {
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push_str(&format!("profile={profile}"));
            }
            if !rule.is_empty() {
                if !s.is_empty() {
                    s.push(' ');
                }
                s.push_str(&format!("rule={rule}"));
            }
            s
        }
        "orbit.friction.reported" => {
            let task_id = getf("task_id");
            let agent = getf("agent");
            let model = getf("model");
            let summary = getf("summary");
            let mut s = format!("friction reported on {task_id}");
            if !agent.is_empty() || !model.is_empty() {
                s.push_str(&format!(" by {agent}/{model}"));
            }
            if !summary.is_empty() {
                s.push_str(&format!(": {summary}"));
            }
            s
        }
        "orbit.job.step_started" => {
            format!(
                "step {} started [run={}]",
                getf("step_id"),
                getf("job_run_id"),
            )
        }
        "orbit.job.step_finished" => {
            let step = getf("step_id");
            let outcome = getf("outcome");
            let success = fields.get("success").and_then(Value::as_bool);
            match success {
                Some(true) => format!("step {step} finished ok ({outcome})"),
                Some(false) => format!("step {step} finished {outcome}"),
                None => format!("step {step} finished {outcome}"),
            }
        }
        "orbit.job.step_retry" => format!(
            "step {} retry attempt={} backoff_ms={}",
            getf("step_id"),
            getn("attempt"),
            getn("next_backoff_ms"),
        ),
        "orbit.job.step_skipped" => {
            format!("step {} skipped: {}", getf("step_id"), getf("reason"))
        }
        "orbit.job.step_denied" => {
            format!("step {} denied: {}", getf("step_id"), getf("reason"))
        }
        "orbit.job.fanout" => format!(
            "fanout phase={} step={} workers={} collected={} failed={}",
            getf("phase"),
            getf("step_id"),
            getn("worker_count"),
            getn("collected"),
            getn("failed"),
        ),
        "orbit.job.worker_state" => format!(
            "worker[{}] state={} step={}",
            getn("worker_index"),
            getf("state"),
            getf("step_id"),
        ),
        "orbit.job.loop_iteration" => format!(
            "loop {} phase={} step={}",
            getn("iteration"),
            getf("phase"),
            getf("step_id"),
        ),
        "orbit.job.loop_did_not_converge" => format!(
            "loop step={} did not converge after {} iterations",
            getf("step_id"),
            getn("max_iterations"),
        ),
        AGENT_OUTPUT_TARGET => {
            let stream = getf("stream");
            let line = getf("line");
            if !stream.is_empty() {
                format!("[{stream}] {line}")
            } else {
                line.to_string()
            }
        }
        _ => {
            // Generic fallback: render fields as `key=value` space-separated,
            // omitting `message` (already handled above for known targets) and
            // `target` (already in the source column).
            let mut parts: Vec<String> = Vec::new();
            if let Value::Object(map) = fields {
                if let Some(message) = map.get("message").and_then(Value::as_str) {
                    parts.push(message.to_string());
                }
                for (k, v) in map {
                    if k == "message" {
                        continue;
                    }
                    let value_str = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    parts.push(format!("{k}={value_str}"));
                }
            }
            parts.join(" ")
        }
    }
}

fn colorize_source(target: &str, label: &str) -> ColoredString {
    match target {
        "orbit.policy.deny" => label.red().bold(),
        "orbit.friction.reported" => label.yellow(),
        t if t.starts_with("orbit.job.") => label.cyan(),
        AGENT_OUTPUT_TARGET => label.magenta(),
        _ => label.normal(),
    }
}

fn colorize_code(target: &str, level: &str, code: &str) -> ColoredString {
    if target == "orbit.policy.deny" {
        return code.red().bold();
    }
    match level {
        "ERROR" => code.red().bold(),
        "WARN" => code.yellow(),
        "INFO" => code.green(),
        "DEBUG" => code.blue(),
        _ => code.normal(),
    }
}
