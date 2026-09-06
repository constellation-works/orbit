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

pub use antigravity::{antigravity_terminal_error_diagnostic, apply_antigravity_print_timeout};

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
/// so usage, tool traffic, and provider failures remain observable. Codex and
/// Copilot need a narrower view because their JSONL streams also contain
/// reasoning and tool payloads that may quote an unrelated Orbit envelope.
/// Other providers retain their existing normalized response boundary.
/// [ORB-11348]
pub fn project_cli_response<'a>(provider: &str, stdout: &'a [u8]) -> Cow<'a, [u8]> {
    match provider {
        "codex" => {
            codex::project_codex_response(stdout).map_or_else(|| Cow::Borrowed(stdout), Cow::Owned)
        }
        "copilot" => Cow::Owned(copilot::project_copilot_response(stdout)),
        _ => normalize_cli_stdout(provider, stdout),
    }
}
