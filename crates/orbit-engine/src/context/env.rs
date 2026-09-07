//! Subprocess provenance environment variables shared by every engine spawn
//! path.
//!
//! The `env_set` / `ORBIT_*` run-state resolution that used to live here served
//! the v1 executor transport and was deleted with it in [ORB-10395]; v2 dispatch
//! builds its child environment in `crate::activity_job::cli_runner`.
//!
//! Parent-environment admission of `ORBIT_*` names is the explicit allowlist in
//! `orbit_common::security::child_env`, not a prefix wildcard. This module
//! builds only the provenance subset of that list: `ORBIT_RUN_ID`,
//! `ORBIT_MANAGED_RUN_CONTEXT`, `ORBIT_AGENT_NAME`, `ORBIT_AGENT_MODEL`,
//! `ORBIT_SESSION_ID`, `ORBIT_TASK_ID`, `ORBIT_ACTIVE_TASK_ID`. Locators and
//! activity bindings (`ORBIT_ROOT`, `ORBIT_REGISTRY_ROOT`, `ORBIT_WORKSPACE`,
//! `ORBIT_WORKTREE_ROOT`, `ORBIT_BIN`, `ORBIT_STEP_INDEX`,
//! `ORBIT_TASK_ACTOR_KIND`, `ORBIT_ACTIVITY_*`, `ORBIT_SEARCH_COMPANION*`) are
//! injected by the CLI runner or forwarded from the parent when present.

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
        vars.push(("ORBIT_RUN_ID".to_string(), run_id.to_string()));
    }
    if config.orbit_managed_run_context {
        vars.push(("ORBIT_MANAGED_RUN_CONTEXT".to_string(), "1".to_string()));
    }
    if let Some(agent_name) = config.orbit_agent_name {
        vars.push(("ORBIT_AGENT_NAME".to_string(), agent_name.to_string()));
    }
    if let Some(model) = config.orbit_agent_model {
        vars.push(("ORBIT_AGENT_MODEL".to_string(), model.to_string()));
    }
    if let Some(session_id) = config.orbit_session_id {
        vars.push(("ORBIT_SESSION_ID".to_string(), session_id.to_string()));
    }
    if let Some(task_id) = config.orbit_task_id {
        vars.push(("ORBIT_TASK_ID".to_string(), task_id.to_string()));
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
