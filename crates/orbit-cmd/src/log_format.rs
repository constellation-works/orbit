//! Plain-text source and code columns for log events, shared by `orbit log`
//! and the dashboard's log view so both label one event the same way.

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
