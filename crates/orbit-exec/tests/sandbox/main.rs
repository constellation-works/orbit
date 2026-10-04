//! Platform sandbox enforcement: Linux Landlock and bwrap, and macOS
//! `sandbox-exec`. Each module is compiled only on its platform.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod linux_landlock;
mod linux_sandbox;
mod macos_sandbox;
