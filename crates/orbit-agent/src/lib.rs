#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy public provider surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
//! Provider CLI runtimes and audit contracts for Orbit.
//!
//! Provider adapters build command descriptors (program, arguments and stdin)
//! that `orbit-engine` executes through `orbit-exec`. Response helpers project
//! provider stdout into Orbit envelopes and diagnostics.
//!
//! [`loop_engine::audit`] retains the structured events and redacted blob sinks
//! that the engine persists, including historical event variants.
//!
//! # Dependency direction
//! `orbit-common` / `orbit-types` → `orbit-agent` → `orbit-engine`

mod agent;
pub mod loop_engine;
pub mod providers;
mod runtime;
mod types;

pub use agent::{Agent, AgentConfig};
pub use providers::{
    antigravity_background_task_diagnostic, antigravity_print_timeout_diagnostic,
    antigravity_terminal_error_diagnostic, apply_antigravity_print_timeout,
    latest_assistant_message, normalize_cli_stdout, project_cli_response, provider_usage_windows,
};
pub use types::{AgentOperation, AgentRequest, AgentResponseStatus};
pub use types::{
    DeclaredResponseFailure, ParsedStdout, provider_authentication_failure,
    provider_capacity_exhausted, provider_content_refusal, provider_invocation_diagnostic,
    provider_usage_limit, provider_usage_limit_details,
};
