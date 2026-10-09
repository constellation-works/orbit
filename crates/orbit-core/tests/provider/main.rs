//! Provider CLI backends driven against fake agent executables.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

orbit_common::isolate_test_process!();

mod antigravity_fake_agent;
mod claude_fake_agent;
mod copilot_fake_agent;
mod cursor_fake_agent;
mod grok_cli_backend_smoke;
mod opencode_fake_agent;
mod pi_fake_agent;
