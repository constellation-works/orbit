//! The `RuntimeHost` implementation for `OrbitRuntime`: the single trait impl in
//! `host`, with its method bodies grouped by concern beside it. A trait can be
//! implemented once per type, so `host` keeps every method as a delegation.

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
