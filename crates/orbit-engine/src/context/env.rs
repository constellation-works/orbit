//! Subprocess provenance environment variables shared by every engine spawn
//! path.
//!
//! The `env_set` / `ORBIT_*` run-state resolution that used to live here served
//! the v1 executor transport and was deleted with it in [ORB-10395]; v2 dispatch
//! builds its child environment in the CLI runner.
//!
//! Parent-environment admission of `ORBIT_*` names is the explicit allowlist in
//! `orbit_common::security::child_env`, not a prefix wildcard. This module
//! builds only the provenance subset of that list: `ORBIT_RUN_ID`,
//! `ORBIT_MANAGED_RUN_CONTEXT`, `ORBIT_AGENT_NAME`, `ORBIT_AGENT_MODEL`,
//! `ORBIT_SESSION_ID`, `ORBIT_TASK_ID`, `ORBIT_ACTIVE_TASK_ID`. Locators and
//! activity bindings (`ORBIT_ROOT`, `ORBIT_REGISTRY_ROOT`, `ORBIT_WORKSPACE`,
//! `ORBIT_WORKTREE_ROOT`, `ORBIT_SCRATCH_DIR`, `ORBIT_BIN`, `ORBIT_STEP_INDEX`,
//! `ORBIT_TASK_ACTOR_KIND`, `ORBIT_ACTIVITY_*`) are
//! injected by the CLI runner or forwarded from the parent when present.

use orbit_common::env_vars;
use std::path::Path;

use orbit_common::security::child_env::{
    MCP_MANAGED_CONTEXT_ENV, MCP_MANAGED_RUN_ID_ENV, MCP_MANAGED_SESSION_ID_ENV,
};

/// Route automatic Python cache writes to a caller-owned scratch location.
/// Keep an explicit prefix admitted by the child's environment policy.
pub(crate) fn default_python_cache_prefix(env: &mut Vec<(String, String)>, prefix: &Path) {
    const VAR: &str = "PYTHONPYCACHEPREFIX";
    if env
        .iter()
        .any(|(key, value)| key == VAR && !value.is_empty())
    {
        return;
    }
    env.retain(|(key, _)| key != VAR);
    env.push((VAR.to_string(), prefix.to_string_lossy().into_owned()));
}

#[derive(Debug, Default)]
pub(crate) struct ProvenanceEnv<'a> {
    pub(crate) orbit_run_id: Option<&'a str>,
    pub(crate) orbit_managed_run_context: bool,
    pub(crate) orbit_agent_name: Option<&'a str>,
    pub(crate) orbit_agent_model: Option<&'a str>,
    pub(crate) orbit_session_id: Option<&'a str>,
    pub(crate) orbit_task_id: Option<&'a str>,
    pub(crate) orbit_active_task: bool,
    pub(crate) agent_run_id: Option<&'a str>,
    pub(crate) agent_model: Option<&'a str>,
    pub(crate) agent_task_id: Option<&'a str>,
}

/// Builds subprocess provenance variables shared by every engine spawn path.
///
/// The namespaces deliberately remain separate: `ORBIT_*` is internal
/// plumbing whose consumers depend on Orbit job-run semantics, while
/// `AGENT_*` is a spawner-neutral, cross-repository commit-telemetry contract
/// whose model value must be the exact provider model string. Callers select
/// only the fields their spawn site historically emitted.
pub(crate) fn provenance_env(config: ProvenanceEnv<'_>) -> Vec<(String, String)> {
    let mut vars = Vec::new();

    if let Some(run_id) = config.orbit_run_id {
        vars.push((MCP_MANAGED_RUN_ID_ENV.to_string(), run_id.to_string()));
    }
    if config.orbit_managed_run_context {
        vars.push((MCP_MANAGED_CONTEXT_ENV.to_string(), "1".to_string()));
    }
    if let Some(agent_name) = config.orbit_agent_name {
        vars.push((
            env_vars::ORBIT_AGENT_NAME.to_string(),
            agent_name.to_string(),
        ));
    }
    if let Some(model) = config.orbit_agent_model {
        vars.push((env_vars::ORBIT_AGENT_MODEL.to_string(), model.to_string()));
    }
    if let Some(session_id) = config.orbit_session_id {
        vars.push((
            MCP_MANAGED_SESSION_ID_ENV.to_string(),
            session_id.to_string(),
        ));
    }
    if let Some(task_id) = config.orbit_task_id {
        vars.push((env_vars::ORBIT_TASK_ID.to_string(), task_id.to_string()));
        if config.orbit_active_task {
            vars.push(("ORBIT_ACTIVE_TASK_ID".to_string(), task_id.to_string()));
        }
    }
    if let Some(run_id) = config.agent_run_id {
        vars.push(("AGENT_RUN_ID".to_string(), run_id.to_string()));
    }
    if let Some(model) = config.agent_model {
        vars.push(("AGENT_MODEL".to_string(), model.to_string()));
    }
    if let Some(task_id) = config.agent_task_id {
        vars.push(("AGENT_TASK".to_string(), task_id.to_string()));
    }

    vars
}
