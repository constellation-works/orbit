//! The per-request usage prompt-size price tiers key on. [ORB-19680]
//!
//! A tier prices one model request by its own prompt, so invocation totals
//! cannot say which requests reached it. Codex writes each request's
//! `last_token_usage` to its session rollout under `CODEX_HOME`. Claude's
//! message stream shows each request's prompt but not its final output, so it
//! can only show that no request reached a tier. Other providers report only
//! totals, and record none.

use std::path::Path;

use chrono::{DateTime, Utc};
use orbit_types::telemetry::TokenUsage;

use super::claude::claude_long_prompt_requests;
use super::codex::codex_long_prompt_requests;

/// The usage of each model request of one `provider` run whose prompt may
/// reach a prompt-size price tier, for `InvocationTrace::long_prompt_requests`.
///
/// `stdout` is the run's capture; `stdout_complete` is false when it was
/// truncated, which may have dropped Claude messages but leaves Codex's
/// rollout whole. `codex_home` is the `CODEX_HOME` the Codex child
/// ran with, and `model` the provider-reported model the invocation is priced
/// as. `None` means the run's per-request usage is unknown; an empty list
/// means it is known and no request reached the floor.
#[must_use]
pub fn provider_long_prompt_requests(
    provider: &str,
    stdout: &[u8],
    stdout_complete: bool,
    codex_home: Option<&Path>,
    model: Option<&str>,
    now: DateTime<Utc>,
) -> Option<Vec<TokenUsage>> {
    match provider {
        "claude" if stdout_complete => claude_long_prompt_requests(stdout, model),
        "codex" => codex_long_prompt_requests(stdout, codex_home?, now),
        _ => None,
    }
}
