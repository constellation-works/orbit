//! The composed runtime through its public surface: dispatch admission, the
//! distributed drain, `os:` tag routing, retired deterministic stubs, final
//! recovery, relation auto-close, PR closure on terminal decisions and the
//! sandbox opt-out.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod dispatch_admission;
mod distributed_drain;
mod drain_approval;
mod final_recovery;
mod relation_auto_close;
mod retired_stubs;
mod review_gate_audit;
mod sandbox_off;
mod session_events;

mod host_os_routing;
mod host_resources;

mod task_pilot;
mod task_pr_closure;
