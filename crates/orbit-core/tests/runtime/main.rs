//! The composed runtime through its public surface: dispatch admission, the
//! distributed drain, `os:` tag routing, retired deterministic stubs, final
//! recovery, repeated rebase recovery, relation auto-close, PR closure on
//! terminal decisions, plugin inspection, the sandbox opt-out and cold lexical
//! search hydration.
//!
//! One integration-test binary per area keeps link cost down; add a module
//! here rather than a new top-level `tests/*.rs` file
//! (`docs/design-patterns/test_strategy.md`).

// Integration fixtures unwrap setup invariants.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod actor_identity;
mod artifact_tools;
mod dispatch_admission;
mod distributed_drain;
mod drain_approval;
mod drain_cancel;
mod final_recovery;
mod forge_hold_resume;
mod held_child_guard;
mod rebase_recovery_attempts;
mod relation_auto_close;
mod retired_stubs;
mod review_baseline_hold;
mod review_continuation;
#[cfg(target_os = "linux")]
mod review_evidence_fulfilment;
mod review_evidence_paths;
mod review_evidence_writers;
mod review_gate_audit;
mod review_held_resume;
mod review_record_ids;
mod review_report_revisions;
mod sandbox_off;
mod search_hydration;
mod security_alert_sweep;
mod session_events;
mod shared_root_identity;
mod step_recovery;

mod host_os_routing;
mod host_resources;
mod implement_blocker;
mod invocation_metrics;
mod job_finalization;
mod local_route_before_pr;
mod plugin_inspection;
mod pr_forge_admission;
mod provider_failure_hold;

mod task_delivery;
mod task_pilot;
mod task_pr_closure;
mod task_update;
mod upgrade_resume;
