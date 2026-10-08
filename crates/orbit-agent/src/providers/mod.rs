//! Provider CLI adapters and stdout projection helpers.
//!
//! Adapters build command descriptors and stdin envelopes that the engine
//! executes through `orbit-exec`.

pub(crate) mod antigravity;
pub(crate) mod claude;
pub(crate) mod codex;
mod common;
pub(crate) mod copilot;
pub(crate) mod cursor;
pub(crate) mod gemini;
pub(crate) mod grok;
pub(crate) mod mock_agent;
pub(crate) mod ollama;
pub(crate) mod opencode;
pub(crate) mod pi;

#[cfg(test)]
mod tests;

pub use antigravity::{
    antigravity_print_timeout_diagnostic, antigravity_terminal_error_diagnostic,
    apply_antigravity_print_timeout,
};

use std::borrow::Cow;

use crate::types::AgentInvocationSpec;
use serde_json::Value;

/// Builds the `AgentInvocationSpec` for a provider, combining CLI args and stdin.
/// All three runtimes share this same structure; only the provider differs.
pub(crate) fn build_invocation_spec(
    runtime_key: &'static str,
    required_env_vars: &'static [&'static str],
    command: String,
    args: Vec<String>,
    stdin: Vec<u8>,
) -> AgentInvocationSpec {
    AgentInvocationSpec {
        runtime_key,
        program: command,
        args,
        stdin,
        stdout_schema_json: None,
        required_env_vars,
        fixed_env: &[],
    }
}

/// Normalize a provider's raw stdout for invocation tracing and diagnostics.
///
/// Most providers emit their Orbit envelope directly and are returned
/// borrowed and unchanged. Provider adapters remove input echoes and
/// unsupported control-plane frames while retaining the provider-authored
/// material needed for telemetry. Response/status projection applies the
/// stricter [`project_cli_response`] boundary afterward.
/// [ORB-10946] [ORB-10945] [ORB-11295]
///
/// `provider` is the resolved canonical provider id. An unrecognized id is
/// not an error here: normalization is a per-provider accommodation, and the
/// default of "read stdout as-is" is what every other provider needs.
pub fn normalize_cli_stdout<'a>(provider: &str, stdout: &'a [u8]) -> Cow<'a, [u8]> {
    match provider {
        "copilot" => Cow::Owned(copilot::normalize_copilot_stdout(stdout)),
        "cursor" => Cow::Owned(cursor::normalize_cursor_stdout(stdout)),
        "pi" => Cow::Owned(pi::normalize_pi_stdout(stdout)),
        "antigravity" | "agy" => Cow::Owned(antigravity::normalize_antigravity_stdout(stdout)),
        "opencode" => Cow::Owned(opencode::normalize_opencode_stdout(stdout)),
        _ => Cow::Borrowed(stdout),
    }
}

/// Expose only provider-attributed assistant answer content to Orbit's
/// response-envelope projection.
///
/// Invocation traces and diagnostics continue to use [`normalize_cli_stdout`]
/// so usage, tool traffic, and provider failures remain observable. Codex,
/// Copilot, and Grok need a narrower view because their provider wrappers can
/// carry reasoning and tool payloads that may quote an unrelated Orbit
/// envelope. Other providers retain their existing normalized response
/// boundary.
/// [ORB-11348]
pub fn project_cli_response<'a>(provider: &str, stdout: &'a [u8]) -> Cow<'a, [u8]> {
    match provider {
        "codex" => {
            codex::project_codex_response(stdout).map_or_else(|| Cow::Borrowed(stdout), Cow::Owned)
        }
        "copilot" => Cow::Owned(copilot::project_copilot_response(stdout)),
        "grok" => {
            grok::project_grok_response(stdout).map_or_else(|| Cow::Borrowed(stdout), Cow::Owned)
        }
        _ => normalize_cli_stdout(provider, stdout),
    }
}

/// The newest assistant message `stdout` carries, or `None` when it has none.
///
/// Starts from the provider's answer projection, so tool traffic, reasoning,
/// and input echoes never count as a message. Within it, the newest recognized
/// frame wins: an Orbit response envelope (returned verbatim), a `result`
/// wrapper, or an `assistant` message's text blocks. Output that is not JSON
/// at all is itself the message. The text is unbounded; callers bound it.
pub fn latest_assistant_message(provider: &str, stdout: &[u8]) -> Option<String> {
    let answer = project_cli_response(provider, stdout);
    let text = String::from_utf8_lossy(answer.as_ref());
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // A single wrapper document (`--output-format json`), or a projection that
    // already reduced the stream to the answer text.
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return Some(message_from_document(&value, text).unwrap_or_else(|| text.to_string()));
    }
    let mut saw_json = false;
    for line in text.lines().rev() {
        let line = line.trim();
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        saw_json = true;
        if let Some(message) = message_from_document(&value, line) {
            return Some(message);
        }
    }
    (!saw_json).then(|| text.to_string())
}

fn message_from_document(value: &Value, raw: &str) -> Option<String> {
    let object = value.as_object()?;
    if object.contains_key("schemaVersion") && object.contains_key("status") {
        return Some(raw.to_string());
    }
    match object.get("type").and_then(Value::as_str)? {
        "result" => object
            .get("result")
            .and_then(Value::as_str)
            .filter(|result| !result.trim().is_empty())
            .map(str::to_string)
            .or_else(|| {
                object
                    .get("structured_output")
                    .filter(|output| output.is_object())
                    .map(Value::to_string)
            }),
        "assistant" => {
            let text = object
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)?
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then_some(text)
        }
        _ => None,
    }
}
