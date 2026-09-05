//! Concrete agent provider implementations.
//!
//! Two families live here:
//!
//! - **CLI transports** (`claude`, `codex`, `copilot`, `cursor-agent`, `gemini`,
//!   `agy`, `grok`, `ollama`, `opencode`, `pi`, `mock_agent`):
//!   translate an [`AgentRequest`] into a CLI command invocation and stdin
//!   envelope that the engine runs via `orbit-exec`.
//! - **HTTP transports** (`anthropic`, `openai_compat`, `gemini_http`): implement the sibling
//!   [`LoopTransport`](crate::loop_engine::LoopTransport) trait against a
//!   provider's HTTP API. Used by [`AgentLoop`](crate::loop_engine::AgentLoop)
//!   with explicit guardrails, allowlist enforcement, and audit wiring.
//!
//! The two families coexist: adding an HTTP transport does not remove the
//! existing CLI path, and the shared `AgentRuntime` trait is unchanged.

pub mod anthropic;
pub(crate) mod antigravity;
pub(crate) mod claude;
pub(crate) mod codex;
mod common;
pub(crate) mod copilot;
pub(crate) mod cursor;
pub(crate) mod gemini;
pub mod gemini_http;
pub(crate) mod grok;
pub(crate) mod mock_agent;
pub(crate) mod ollama;
pub mod openai_compat;
pub(crate) mod opencode;
pub(crate) mod pi;

use std::borrow::Cow;

use crate::types::AgentInvocationSpec;

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
    }
}

/// Reduce a provider's raw stdout to the bytes Orbit's response/envelope
/// contract may read.
///
/// Most providers emit their Orbit envelope directly and are returned
/// borrowed and unchanged. `copilot` streams JSONL agent events that replay
/// Orbit's own prompt back as a `user.message` frame, so its stream is
/// reduced to model-authored frames first — see the `copilot::copilot_stream`
/// module docs for why that reduction is a correctness requirement rather
/// than tidying. `cursor` wraps the assistant response in a terminal JSON
/// result object; its adapter validates that wrapper and exposes only the
/// model-authored `result` string. `opencode` emits NDJSON events whose
/// `tool_use` and `reasoning` frames can replay Orbit's own prompt, so its
/// adapter keeps only the assistant `text` parts.
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
