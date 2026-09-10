//! Runtime helpers used by the unified `RuntimeHost` implementation.
//!
//! The trait surface is deliberately small: orbit-core owns deterministic
//! action dispatch (which needs the live `ToolContext` + tool registry),
//! provider credential sourcing (env / config access), and the CLI-command
//! resolution for `backend: cli` (workspace-scoped env / config overrides).
//! HTTP agent-loop transport and CLI subprocess execution both live in
//! `orbit-engine`, so this module never names orbit-agent types.

pub(super) mod auto_admission;
pub(super) mod backlog_exclusion;
pub(super) mod child_dispatch;
pub(super) mod ci_failure_admission;
pub(super) mod ci_failure_tasks;
pub(super) mod cli_executor;
pub(super) mod dependabot_alert_tasks;
pub(super) mod dispatch;
pub(super) mod duplicate_tasks;
pub(super) mod leaf_occupancy;
pub(super) mod pipeline_actions;
pub(super) mod sandbox;
pub(super) mod scan_unresolved;
pub(super) mod task_context;
pub(super) mod task_pilot;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
pub(super) mod triage;
pub(super) mod workspace_auto;

#[cfg(test)]
use crate::OrbitRuntime;

#[cfg(test)]
use orbit_engine::RuntimeHost;

#[cfg(test)]
use serde_json::Value;
