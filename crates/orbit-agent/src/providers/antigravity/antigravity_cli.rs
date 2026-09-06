use std::time::Duration;

use crate::providers::common::render_prompt_with_embedded_envelope;
use crate::types::response_envelope_json_schema_arg;
use orbit_types::identity::ReasoningEffort;

/// Leave this much of the remaining Orbit spawn deadline for outer process
/// supervision after `agy --print-timeout` fires, so a terminal `result` can
/// be written before process-group kill. [ORB-11337]
pub(crate) const PRINT_TIMEOUT_SHUTDOWN_MARGIN: Duration = Duration::from_secs(30);
const PRINT_TIMEOUT_FLAG: &str = "--print-timeout";
const PRINT_TIMEOUT_EQUALS_PREFIX: &str = "--print-timeout=";
const MIN_PRINT_TIMEOUT: Duration = Duration::from_secs(1);

/// Per-request command construction for Antigravity CLI (`agy`).
///
/// Static headless flags live on the shipped executor. This transport adds
/// only the model, effort, and generated envelope schema for one turn.
/// `--print-timeout` is merged later from the remaining spawn deadline so a
/// custom executor can keep a shorter explicit value without duplicating the
/// flag. [ORB-11299] [ORB-11337]
pub(crate) struct AntigravityCliTransport {
    model: Option<String>,
    reasoning_effort: Option<ReasoningEffort>,
}

impl AntigravityCliTransport {
    pub(crate) fn new(model: Option<String>, reasoning_effort: Option<ReasoningEffort>) -> Self {
        Self {
            model,
            reasoning_effort,
        }
    }

    pub(crate) fn args(&self) -> Vec<String> {
        let mut args = Vec::new();
        args.push("--json-schema".to_string());
        args.push(response_envelope_json_schema_arg());
        if let Some(model) = &self.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(effort) = self.reasoning_effort {
            args.push("--effort".to_string());
            args.push(effort.to_string());
        }
        args
    }

    /// Documented stream-json stdin: one `user` event, then the caller closes
    /// the pipe. Putting the envelope on `-p` would leak task context into
    /// process listings and audit argv.
    pub(crate) fn stdin(&self, envelope_json: &[u8]) -> Vec<u8> {
        let prompt = render_prompt_with_embedded_envelope(envelope_json);
        let prompt = String::from_utf8_lossy(&prompt);
        let event = serde_json::json!({
            "event": "user",
            "message": { "content": prompt.as_ref() },
        });
        // String content serializes to a JSON object; this cannot fail.
        let mut bytes = serde_json::to_vec(&event)
            .unwrap_or_else(|_| br#"{"event":"user","message":{"content":""}}"#.to_vec());
        bytes.push(b'\n');
        bytes
    }

    pub(crate) fn model_name(&self) -> Option<&str> {
        self.model.as_deref()
    }
}

/// Derive documented `agy --print-timeout` from the remaining Orbit spawn
/// deadline. A 30s shutdown margin keeps outer supervision authoritative.
/// Budgets that do not fit the margin still get an explicit value so the
/// documented 5m default cannot apply.
pub(crate) fn derived_antigravity_print_timeout(remaining_deadline: Duration) -> Duration {
    let remaining = remaining_deadline.max(MIN_PRINT_TIMEOUT);
    if remaining > PRINT_TIMEOUT_SHUTDOWN_MARGIN {
        remaining - PRINT_TIMEOUT_SHUTDOWN_MARGIN
    } else {
        remaining
    }
}

/// Format a duration as the Go `time.ParseDuration` spelling `agy` documents
/// (`2h59m30s`, `15m`, `30s`).
pub(crate) fn format_antigravity_print_timeout(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    if hours > 0 {
        format!("{hours}h{minutes}m{seconds}s")
    } else if minutes > 0 {
        if seconds == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m{seconds}s")
        }
    } else if total_secs > 0 {
        format!("{seconds}s")
    } else {
        format!("{}ms", duration.as_millis().max(1))
    }
}

/// Merge `--print-timeout` into combined executor + invocation argv.
///
/// No-ops for other providers. A shorter explicit executor value is kept; a
/// longer one is capped to the derived budget; the flag is never duplicated.
pub fn apply_antigravity_print_timeout(
    provider: &str,
    args: &mut Vec<String>,
    remaining_deadline: Duration,
) {
    if provider != "antigravity" && provider != "agy" {
        return;
    }
    let derived = derived_antigravity_print_timeout(remaining_deadline);
    let chosen = existing_print_timeout(args)
        .map(|existing| existing.min(derived))
        .unwrap_or(derived);
    set_print_timeout(args, chosen);
}

fn existing_print_timeout(args: &[String]) -> Option<Duration> {
    let mut idx = 0;
    while idx < args.len() {
        let arg = &args[idx];
        if let Some(value) = arg.strip_prefix(PRINT_TIMEOUT_EQUALS_PREFIX) {
            return parse_go_duration(value);
        }
        if arg == PRINT_TIMEOUT_FLAG {
            let value = args.get(idx + 1).filter(|next| !next.starts_with('-'))?;
            return parse_go_duration(value);
        }
        idx += 1;
    }
    None
}

fn set_print_timeout(args: &mut Vec<String>, timeout: Duration) {
    let formatted = format_antigravity_print_timeout(timeout);
    let mut idx = 0;
    let mut seen = false;
    while idx < args.len() {
        if args[idx].starts_with(PRINT_TIMEOUT_EQUALS_PREFIX) {
            if seen {
                args.remove(idx);
                continue;
            }
            args[idx] = format!("{PRINT_TIMEOUT_EQUALS_PREFIX}{formatted}");
            seen = true;
            idx += 1;
            continue;
        }
        if args[idx] == PRINT_TIMEOUT_FLAG {
            if seen {
                if idx + 1 < args.len() && !args[idx + 1].starts_with('-') {
                    args.remove(idx + 1);
                }
                args.remove(idx);
                continue;
            }
            if idx + 1 < args.len() && !args[idx + 1].starts_with('-') {
                args[idx + 1] = formatted.clone();
                seen = true;
                idx += 2;
            } else {
                args.insert(idx + 1, formatted.clone());
                seen = true;
                idx += 2;
            }
            continue;
        }
        idx += 1;
    }
    if !seen {
        args.push(PRINT_TIMEOUT_FLAG.to_string());
        args.push(formatted);
    }
}

fn parse_go_duration(raw: &str) -> Option<Duration> {
    let mut rest = raw.trim();
    if rest.is_empty() {
        return None;
    }
    if let Some(stripped) = rest.strip_prefix('+') {
        rest = stripped;
    }
    if rest.starts_with('-') {
        return None;
    }
    let mut total = Duration::ZERO;
    let mut parsed_any = false;
    while !rest.is_empty() {
        let (number, after_number) = parse_leading_number(rest)?;
        let (unit, after_unit) = parse_leading_unit(after_number)?;
        let contrib = duration_from_unit(number, unit)?;
        total = total.checked_add(contrib)?;
        rest = after_unit;
        parsed_any = true;
    }
    parsed_any.then_some(total)
}

fn parse_leading_number(raw: &str) -> Option<(f64, &str)> {
    let mut end = 0;
    let bytes = raw.as_bytes();
    let mut seen_digit = false;
    let mut seen_dot = false;
    while end < bytes.len() {
        match bytes[end] {
            b'0'..=b'9' => {
                seen_digit = true;
                end += 1;
            }
            b'.' if !seen_dot => {
                seen_dot = true;
                end += 1;
            }
            _ => break,
        }
    }
    if !seen_digit {
        return None;
    }
    let number: f64 = raw[..end].parse().ok()?;
    Some((number, &raw[end..]))
}

fn parse_leading_unit(raw: &str) -> Option<(&'static str, &str)> {
    for unit in ["ms", "us", "µs", "μs", "ns", "h", "m", "s"] {
        if let Some(rest) = raw.strip_prefix(unit) {
            return Some((unit, rest));
        }
    }
    None
}

fn duration_from_unit(number: f64, unit: &str) -> Option<Duration> {
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    let seconds = match unit {
        "h" => number * 3600.0,
        "m" => number * 60.0,
        "s" => number,
        "ms" => number / 1000.0,
        "us" | "µs" | "μs" => number / 1_000_000.0,
        "ns" => number / 1_000_000_000.0,
        _ => return None,
    };
    Duration::try_from_secs_f64(seconds).ok()
}
