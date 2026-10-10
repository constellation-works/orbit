#![deny(clippy::print_stderr, clippy::print_stdout)]
// Legacy policy surfaces still need a focused documentation pass.
#![allow(missing_docs)]
// Unit tests use unwrap/expect for fixture setup; production call sites remain linted.
#![cfg_attr(test, allow(clippy::expect_used, clippy::unwrap_used))]
//! Filesystem-profile policy evaluation for Orbit runtime activities.
//!
//! # Role
//! Consumed by `orbit-core` and `orbit-tools` to interpret the fsProfile-only
//! `PolicyDef` schema and answer "may this profile read/modify this path?"
//!
//! # Key exports
//! - [`PolicyEngine`] — wraps a validated [`orbit_types::policy::PolicyDef`]
//! - [`FsPolicyEvaluation`] — structured allow/deny outcome with matched rule
//! - [`resolve_symlinks`] — symlink-safe path resolution (dangling links
//!   included) shared by policy evaluation and the tools-layer boundary check
//!
//! # Dependency direction
//! `orbit-types` → `orbit-policy` → orbit-core

mod engine;
mod evaluator;

#[cfg(test)]
mod tests;

pub use engine::{FsPolicyEvaluation, PolicyEngine, resolve_symlinks};
