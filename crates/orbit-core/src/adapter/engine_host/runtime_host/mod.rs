//! The `RuntimeHost` implementation for `OrbitRuntime`: one impl per capability
//! trait in `host` (`RunLifecycle`, `ReviewGate`, `HostEnvironment`, `Dispatch`
//! and `Recovery`), with their method bodies grouped by concern beside it. Each
//! body in `host` delegates to `claims`, `validation`, `recovery`,
//! `invocation`, `host_metadata` or another named sibling, or adapts a type on
//! the way through.

pub(in crate::adapter::engine_host) mod activity_tools;
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
