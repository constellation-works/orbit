//! The `RuntimeHost` implementation for `OrbitRuntime`: the single trait impl in
//! `host`, with its method bodies grouped by concern beside it. A trait can be
//! implemented once per type, so `host` holds every method.
//!
//! Residual size: `host.rs` is 1010 lines after extraction. That is above the
//! ~800-line guideline because Rust requires exactly one
//! `impl RuntimeHost for OrbitRuntime` block holding all 133 trait methods;
//! splitting the impl or lowering the count with an assembly macro would
//! change the trait-implementation structure. Most bodies in `host` delegate
//! to `claims`, `validation`, `recovery`, `invocation` or another named
//! sibling, or adapt a type on the way through. Three bodies still hold logic
//! in `host`, and its module docs name them.

mod activity_tools;
mod agent;
mod checkpoints;
mod claims;
mod crew;
mod executors;
mod host;
mod invocation;
mod recovery;
mod task_automation;
mod validation;
