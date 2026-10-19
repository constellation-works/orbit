//! Read-only exploration a managed run delegates through `orbit.agent.invoke`
//! [ORB-16814].
//!
//! An implement or review-repair agent hands a read-only question to a cheap
//! explorer crew and gets a bounded answer back. The explorer runs the
//! sandboxed [`EXPLORE_ACTIVITY`], never the trusted-host activity: it reads
//! the caller's own worktree and cannot write to it.

/// The sandboxed, read-only activity an explorer invocation runs.
pub const EXPLORE_ACTIVITY: &str = "agent_explore";

/// Catalog job that carries one explorer invocation.
pub const EXPLORE_JOB: &str = "agent_explore_pipeline";

/// Activities whose agents may delegate exploration. Every other agent caller
/// of `orbit.agent.invoke` is refused, an explorer included.
pub const EXPLORE_CALLER_ACTIVITIES: &[&str] = &["agent_implement", "agent_review_repair"];

/// Run-input key naming the run that delegated an explorer invocation.
pub const EXPLORE_PARENT_RUN_KEY: &str = "parent_run_id";

/// The inner sandbox an explorer's provider runs under, where the provider
/// has a configurable one and Orbit's executor sandbox does not already wrap
/// it.
pub const EXPLORE_PROVIDER_SANDBOX_MODE: &str = "read-only";
