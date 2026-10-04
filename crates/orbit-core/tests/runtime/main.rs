//! The composed runtime through its public surface: dispatch admission, the
//! distributed drain, relation auto-close and the sandbox opt-out.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod dispatch_admission;
mod distributed_drain;
mod relation_auto_close;
mod sandbox_off;

mod host_resources;

mod task_pilot;
