//! The `RuntimeHost` implementation for `OrbitRuntime`: the single trait impl in
//! `host`, with its method bodies grouped by concern beside it. A trait can be
//! implemented once per type, so `host` holds every method.
//!
//! Residual size: `host/implementation.rs` is 1163 lines. That is above the
//! ~800-line guideline because Rust requires exactly one
//! `impl RuntimeHost for OrbitRuntime` block holding all trait methods;
//! splitting the impl or lowering the count with an assembly macro would
//! change the trait-implementation structure. Each body in `host` delegates
//! to `claims`, `validation`, `recovery`, `invocation`, `host_metadata` or
//! another named sibling, or adapts a type on the way through.

mod activity_tools;
mod agent;
mod auto_task;
mod checkpoints;
mod claims;
mod crew;
mod executors;
mod host;
mod host_metadata;
mod invocation;
mod recovery;
mod recovery_evidence;
mod task_automation;
mod validation;
