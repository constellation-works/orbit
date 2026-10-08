#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy public provider surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
#![allow(
    rustdoc::broken_intra_doc_links,
    rustdoc::invalid_html_tags,
    rustdoc::private_intra_doc_links
)]
//! Agent provider abstraction for Orbit. Two transport families coexist:
//!
//! - **CLI transports** — drive `claude`, `codex`, `gemini`, `grok`, `copilot`,
//!   `cursor-agent`, `ollama`, `opencode`, `pi`, or `mock` as subprocesses
//!   through
//!   [`AgentRuntime`]. Each runtime builds an
//!   [`AgentInvocationSpec`] (program, args, stdin envelope) that the engine
//!   executes through `orbit-exec`; responses are parsed via
//!   [`parse_and_validate_response`].
//! - **HTTP transports** — drive providers directly through the
//!   [`LoopTransport`](loop_engine::LoopTransport) sibling trait. The
//!   provider-agnostic [`AgentLoop`](loop_engine::AgentLoop) runs the
//!   send/parse/dispatch cycle, enforcing guardrails and tool-allowlist rules
//!   and emitting the full structured audit trail via
//!   [`AuditSink`](loop_engine::AuditSink).
//!
//! The two trait shapes differ intentionally — one-shot command descriptor
//! vs. iterative conversation driver — so they coexist instead of being
//! forcibly unified.
//!
//! **Only the CLI path executes Orbit activities.** ORB-10801 retired the
//! `backend: http | cli | auto` selector and the engine's HTTP agent-loop
//! driver, so nothing in an activity asset, job asset, or `config.toml` can
//! reach [`loop_engine`]. It stays as a standalone SDK surface with its own
//! example consumers.
//!
//! # Role
//! Depends on `orbit-types` (shared domain types) and `orbit-tools`
//! (`ToolRegistry` dispatch for HTTP-loop tool calls). Consumed by
//! `orbit-engine`.
//!
//! # Key exports
//! - [`AgentRuntime`] trait and CLI [`Agent`] / [`AgentConfig`] wrappers
//! - [`parse_and_validate_response`] for CLI response envelopes
//! - [`loop_engine::AgentLoop`], [`loop_engine::Session`],
//!   [`loop_engine::LoopTransport`], [`loop_engine::LoopAuditEvent`],
//!   [`loop_engine::AuditSink`] for the standalone HTTP SDK path, which Orbit
//!   activity/job execution does not use (ORB-10801)
//! - [`providers::anthropic::AnthropicMessagesTransport`] — Anthropic HTTP
//!   transport
//! - [`providers::openai_compat::OpenAiCompatTransport`] — OpenAI-compatible
//!   chat-completions HTTP transport for hosted and local endpoints
//! - [`providers::gemini_http::GeminiHttpTransport`] — Google Gemini HTTP
//!   transport with cachedContents caching
//!
//! # Dependency direction
//! `orbit-types` / `orbit-tools` → `orbit-agent` → `orbit-engine`

mod agent;
pub mod loop_engine;
pub mod providers;
mod runtime;
mod types;

pub use agent::{Agent, AgentConfig};
pub use providers::{
    antigravity_print_timeout_diagnostic, antigravity_terminal_error_diagnostic,
    apply_antigravity_print_timeout, latest_assistant_message, normalize_cli_stdout,
    project_cli_response,
};
pub use types::{AgentOperation, AgentRequest, AgentResponseStatus};
pub use types::{
    DeclaredResponseFailure, ParsedStdout, provider_authentication_failure,
    provider_capacity_exhausted, provider_content_refusal, provider_invocation_diagnostic,
};
