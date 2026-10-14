//! Platform sandbox enforcement: Linux Landlock and bwrap, and macOS
//! `sandbox-exec`. Kernel modules compile only on their platform; apply-probe
//! classification and guard-output fixtures run on every platform.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[cfg(target_os = "linux")]
#[path = "../support/git.rs"]
mod git_test_support;

mod linux_landlock;
mod linux_runtime_ipc;
mod linux_sandbox;
mod macos_compile;
mod macos_sandbox;

mod apply_probe;

mod audit_support;
mod build_sandbox;
mod linux_credentials;
mod linux_masks;
mod macos_audit;
mod macos_audit_support;
mod macos_provider;
mod macos_spawn;
mod path_identity;
