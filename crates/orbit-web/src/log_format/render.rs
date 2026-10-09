//! HTML rendering and context shortening for web log events.

use orbit_common::security::redaction::redact_all;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RenderedLogEvent {
    pub ts: String,
    pub source: String,
    pub target: String,
    pub code: String,
    pub level: String,
    pub message_html: String,
    pub agent_stdout: bool,
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
        target: redact_all(target),
        code: format_code(target, level_raw, &fields),
        level: normalize_level(level_raw).to_string(),
        message_html: format_message_html(target, &fields),
        agent_stdout: is_agent_relay(target, &fields)
            && fields.get("stream").and_then(Value::as_str) == Some("stdout"),
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
        .rsplit([':', '.'])
        .next()
        .unwrap_or(target)
        .to_string()
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

/// Copy of `value` with every string scrubbed by [`redact_all`], so a token in
/// any field never reaches rendered HTML. Redaction runs on the raw text,
/// before HTML escaping, the same way the run and incident views do it.
fn redact_strings(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(redact_all(s)),
        Value::Array(items) => Value::Array(items.iter().map(redact_strings).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| (key.clone(), redact_strings(item)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(crate) fn format_message_html(target: &str, fields: &Value) -> String {
    let redacted = redact_strings(fields);
    let fields = &redacted;
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
        _ if is_agent_relay(target, fields) => format_agent_message(fields),
        _ => format_generic_fields(fields),
    }
}

fn is_agent_relay(target: &str, fields: &Value) -> bool {
    matches!(
        target,
        "orbit_engine::activity_job::cli_runner"
            | "orbit_engine::activity_job::cli_runner::supervisor"
    ) && fields.get("line").and_then(Value::as_str).is_some()
}

fn format_agent_message(fields: &Value) -> String {
    let line = fields.get("line").and_then(Value::as_str).unwrap_or("");
    let event = serde_json::from_str::<Value>(line).ok();
    let kind = event
        .as_ref()
        .and_then(|event| event.get("type"))
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty());
    let item_kind = event
        .as_ref()
        .and_then(|event| event.get("item"))
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty());
    let stream = fields
        .get("stream")
        .and_then(Value::as_str)
        .unwrap_or("output");
    // The kind and abbreviated run fit ahead of the context in the narrow dock
    // and status bar. The complete run remains in the tooltip and fields.
    let mut summary = escape_html(&kind.map_or_else(|| format!("agent {stream}"), str::to_string));
    if let Some(run) = fields.get("job_run_id").and_then(Value::as_str) {
        summary.push_str(" · ");
        summary.push_str(&code_value(run.to_string()));
    }
    // After the run so the first 60 characters still carry kind and run.
    if let Some(item_kind) = item_kind {
        summary.push(' ');
        summary.push_str(&escape_html(item_kind));
    }
    // A structured line is already summarised by its kind; echoing the raw
    // payload would put provider JSON in the status bar. A line without a
    // kind is the only record of what the agent said, so it stays.
    let context = if kind.is_some() {
        let mut fields = fields.clone();
        if let Some(map) = fields.as_object_mut() {
            map.remove("line");
        }
        format_generic_fields(&fields)
    } else {
        format_generic_fields(fields)
    };
    if !context.is_empty() {
        summary.push(' ');
        summary.push_str(&context);
    }
    summary
}

fn format_generic_fields(fields: &Value) -> String {
    let mut parts = Vec::new();
    if let Value::Object(map) = fields {
        if let Some(message) = map.get("message").and_then(Value::as_str) {
            parts.push(escape_html(message));
        }
        // Message first; bulky location and run context last, regardless of
        // the tracing serializer's field order.
        for (key, value) in map
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "message" | "cwd" | "job_run_id"))
            .chain(
                ["cwd", "job_run_id"]
                    .into_iter()
                    .filter_map(|key| map.get_key_value(key)),
            )
        {
            let value = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            parts.push(format!("<b>{}</b>={}", escape_html(key), code_value(value)));
        }
    }
    parts.join(" ")
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
    let short = shorten_context_value(&value);
    if short != value {
        format!(
            "<code title=\"{}\">{}</code>",
            escape_html(&value),
            escape_html(&short)
        )
    } else {
        format!("<code>{}</code>", escape_html(&value))
    }
}

fn shorten_context_value(value: &str) -> String {
    let home = std::env::var("HOME").ok().filter(|home| !home.is_empty());
    let relative = home
        .as_deref()
        .and_then(|home| Path::new(value).strip_prefix(home).ok());
    // A temporary home can itself live in a managed checkout. Only worktrees
    // below that home override `~`; an outer checkout is not useful context.
    let path = relative.and_then(Path::to_str).unwrap_or(value);
    if Path::new(value).is_absolute()
        && let Some(worktree) = path
            .rsplit_once("/.orbit/state/worktrees/orbit-")
            .map(|(_, worktree)| worktree)
            .or_else(|| path.strip_prefix(".orbit/state/worktrees/orbit-"))
    {
        return worktree.to_string();
    }
    if value.starts_with("jrun-") {
        let parts: Vec<_> = value.split('-').collect();
        if let ["jrun", date, time, suffix] = parts.as_slice()
            && date.len() == 8
            && time.len() == 4
            && date
                .chars()
                .chain(time.chars())
                .all(|ch| ch.is_ascii_digit())
        {
            return format!("jrun-…-{suffix}");
        }
    }
    if let Some(relative) = relative {
        return if relative.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", relative.display())
        };
    }
    value.to_string()
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
