//! Audit correlation and agent identity: the trusted context and role label
//! a tool call is recorded under.

use std::ffi::OsStr;

use orbit_common::{Env, OrbitError};
use orbit_tools::{ActivityBinding, ReservationOwnerContext};
use orbit_types::identity::{
    normalize_agent_family_for_model, normalize_optional_attribution_label,
};
use orbit_types::task::ReservationOwnerMetadata;
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

use crate::runtime::run_input::{
    managed_dispatch_context_from_env, managed_run_context_from_env,
    managed_run_context_run_id_from_env,
};

use super::execute::ToolEntryPoint;

/// Trusted audit-correlation fields at the MCP/CLI dispatch seam.
#[derive(Debug, Default, Clone)]
pub struct AuditContext {
    pub session_id: Option<String>,
    pub task_id: Option<String>,
    pub job_run_id: Option<String>,
    pub activity_id: Option<String>,
    pub step_index: Option<i64>,
}

fn env_trimmed(value: Option<&OsStr>) -> Option<String> {
    value
        .and_then(OsStr::to_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_non_empty(value: Option<&OsStr>) -> Option<String> {
    value
        .and_then(OsStr::to_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn env_non_blank(value: Option<&OsStr>) -> Option<String> {
    value
        .and_then(OsStr::to_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn resolve_audit_context(
    env: &Env,
    input: &Value,
    entry_point: ToolEntryPoint,
    session_context: Option<&ToolSessionContext>,
) -> AuditContext {
    fn input_str(input: &Value, key: &str) -> Option<String> {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }

    if entry_point == ToolEntryPoint::Mcp {
        // The MCP entry point never reads model-authored tool JSON for
        // correlation; `session_context` is retained in the signature because
        // it is the trusted envelope the caller must hand over to reach it.
        let _ = session_context;
        return trusted_mcp_audit_context_with_env(env);
    }

    AuditContext {
        session_id: None,
        task_id: input_str(input, "task_id").or_else(|| env_trimmed(env.orbit_task_id())),
        job_run_id: input_str(input, "job_run_id")
            .or_else(|| input_str(input, "run_id"))
            .or_else(|| env_trimmed(env.orbit_run_id())),
        activity_id: input_str(input, "activity_id")
            .or_else(|| env_trimmed(env.orbit_activity_id())),
        step_index: input
            .get("step_index")
            .and_then(Value::as_i64)
            .or_else(|| env_trimmed(env.orbit_step_index()).and_then(|s| s.parse().ok())),
    }
}

/// Resolve MCP audit correlation exclusively from the managed process
/// envelope. Model-authored tool JSON and session audit metadata are not
/// correlation inputs.
pub fn trusted_mcp_audit_context(env: &orbit_common::Env) -> AuditContext {
    trusted_mcp_audit_context_with_env(env)
}

fn trusted_mcp_audit_context_with_env(env: &Env) -> AuditContext {
    if managed_dispatch_context_from_env(env) {
        AuditContext {
            session_id: env_trimmed(env.orbit_session_id()),
            task_id: env_trimmed(env.orbit_task_id()),
            job_run_id: managed_run_context_run_id_from_env(env),
            activity_id: env_trimmed(env.orbit_activity_id()),
            step_index: env_trimmed(env.orbit_step_index()).and_then(|value| value.parse().ok()),
        }
    } else {
        AuditContext::default()
    }
}

pub(super) fn reservation_owner_from_env(
    process_env: &orbit_common::Env,
) -> Option<ReservationOwnerContext> {
    managed_run_context_run_id_from_env(process_env).map(|owner_run_id| ReservationOwnerContext {
        owner_metadata: Some(ReservationOwnerMetadata {
            source: Some("orbit_cli".to_string()),
            ..Default::default()
        }),
        owner_run_id,
    })
}

/// The managed activity a nested `orbit tool run` / `orbit mcp serve` call
/// serves, read from the run envelope the dispatching host stamped into the
/// agent's environment — never from tool input. `None` outside a managed run.
pub(super) fn activity_binding_from_env(env: &Env) -> Option<ActivityBinding> {
    let job_run_id = managed_run_context_run_id_from_env(env)?;
    let task_id = env_trimmed(env.orbit_task_id());
    Some(ActivityBinding {
        job_run_id,
        task_id,
    })
}

pub(super) fn managed_run_context(process_env: &orbit_common::Env) -> bool {
    managed_run_context_from_env(process_env)
}

fn read_agent_identity_from_env(env: &Env) -> (Option<String>, Option<String>) {
    let agent = env_non_empty(env.orbit_agent_name());
    let model = env_non_empty(env.orbit_agent_model());
    (agent, model)
}

fn resolve_agent_identity(
    env: &Env,
    agent_override: Option<String>,
    model_override: Option<String>,
) -> Result<(Option<String>, Option<String>), OrbitError> {
    let (env_agent_name, env_model_name) = read_agent_identity_from_env(env);
    let has_override = agent_override
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || model_override
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
    let (agent, model) = if has_override {
        (agent_override, model_override)
    } else {
        (env_agent_name, env_model_name)
    };
    let agent = normalize_agent_family_for_model(agent.as_deref(), model.as_deref())?;
    // Preserve the dispatch-provided model separately from its normalized family.
    // Tool input is resolved later and cannot override this identity.
    Ok((agent, model))
}

pub(super) fn resolve_agent_identity_for_entry_point(
    env: &Env,
    entry_point: ToolEntryPoint,
    agent_override: Option<String>,
    model_override: Option<String>,
) -> Result<(Option<String>, Option<String>), OrbitError> {
    if entry_point == ToolEntryPoint::Mcp && !managed_run_context(env) {
        return Ok((None, None));
    }
    resolve_agent_identity(env, agent_override, model_override)
}

/// Resolve the audit `role` label for a tool invocation.
///
/// Runtime envelope identity (`ORBIT_AGENT_*`) is authoritative for agent
/// activities and overwrites any self-reported `model` field in tool JSON; an
/// explicit `--agent`/`--model` flag overrides the envelope, as in
/// `resolve_agent_identity`, and still outranks tool JSON. Manual CLI/MCP
/// calls without an envelope keep the legacy input/flag precedence (tool JSON
/// `model`, then flags).
pub fn audit_role_label(
    env: &orbit_common::Env,
    input: &Value,
    agent_override: Option<&str>,
    model_override: Option<&str>,
) -> String {
    audit_role_label_with_env(env, input, agent_override, model_override)
}

fn trimmed_non_blank(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn finalize_audit_role_label(agent: Option<String>, model: Option<String>) -> String {
    let agent = normalize_agent_family_for_model(agent.as_deref(), model.as_deref())
        .ok()
        .flatten()
        .or(agent);

    normalize_optional_attribution_label(model.as_deref().or(agent.as_deref()), model.as_deref())
        .unwrap_or_else(|| "agent".to_string())
}

fn audit_role_label_with_env(
    env: &Env,
    input: &Value,
    agent_override: Option<&str>,
    model_override: Option<&str>,
) -> String {
    let (input_agent, input_model) = read_input_identity(input);
    let env_agent = env_non_blank(env.orbit_agent_name());
    let env_model = env_non_blank(env.orbit_agent_model());
    let has_input_identity = input_agent.is_some() || input_model.is_some();
    let flag_agent = trimmed_non_blank(agent_override);
    let flag_model = trimmed_non_blank(model_override);
    let has_flag_identity = flag_agent.is_some() || flag_model.is_some();
    let has_env_identity = env_agent.is_some() || env_model.is_some();
    let (agent, model) = if has_env_identity && !has_flag_identity {
        let agent = normalize_agent_family_for_model(env_agent.as_deref(), env_model.as_deref())
            .ok()
            .flatten()
            .or(env_agent);
        (agent.clone(), agent)
    } else if has_flag_identity && (has_env_identity || !has_input_identity) {
        (flag_agent, flag_model)
    } else if has_input_identity {
        (input_agent, input_model)
    } else {
        (env_agent, env_model)
    };
    finalize_audit_role_label(agent, model)
}

/// Resolve the audit role with the MCP trust boundary applied. Standalone MCP
/// calls are always `unverified`; an authenticated managed envelope may use
/// only its engine-provided identity and never caller JSON/flags.
pub fn audit_role_label_for_entry_point(
    env: &orbit_common::Env,
    input: &Value,
    agent_override: Option<&str>,
    model_override: Option<&str>,
    entry_point: ToolEntryPoint,
) -> String {
    audit_role_label_for_entry_point_with_env(
        env,
        input,
        agent_override,
        model_override,
        entry_point,
    )
}

pub(super) fn audit_role_label_for_entry_point_with_env(
    env: &Env,
    input: &Value,
    agent_override: Option<&str>,
    model_override: Option<&str>,
    entry_point: ToolEntryPoint,
) -> String {
    if entry_point != ToolEntryPoint::Mcp {
        return audit_role_label_with_env(env, input, agent_override, model_override);
    }
    if !managed_run_context(env) {
        return "unverified".to_string();
    }

    let env_agent = env_non_blank(env.orbit_agent_name());
    let env_model = env_non_blank(env.orbit_agent_model());
    let agent = normalize_agent_family_for_model(env_agent.as_deref(), env_model.as_deref())
        .ok()
        .flatten()
        .or(env_agent);
    normalize_optional_attribution_label(
        agent.as_deref().or(env_model.as_deref()),
        env_model.as_deref(),
    )
    .unwrap_or_else(|| "unverified".to_string())
}

/// Server-derived model recorded on an MCP tool audit row.
///
/// Order: the job's resolved model (a brokered run, else managed
/// `ORBIT_AGENT_MODEL`), else the session `--orchestrator` crew's configured
/// model, else MCP `clientInfo.name`. Tool-input `model` is not read. CLI
/// calls return `None`. The value is not an authenticated role.
pub(super) fn mcp_audit_session_model(
    env: &Env,
    entry_point: ToolEntryPoint,
    brokered_model: Option<&str>,
    orchestrator_model: Option<&str>,
    mcp_client_name: Option<&str>,
) -> Option<String> {
    if entry_point != ToolEntryPoint::Mcp {
        return None;
    }
    fn kept(value: Option<&str>) -> Option<String> {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }
    if let Some(model) = kept(brokered_model) {
        return Some(model);
    }
    if managed_run_context(env) {
        let env_model = env.orbit_agent_model().and_then(OsStr::to_str);
        if let Some(model) = kept(env_model) {
            return Some(model);
        }
    }
    kept(orchestrator_model).or_else(|| kept(mcp_client_name))
}

/// The run's agent family as a brokered call carries it, taken from the
/// run's dispatch record rather than this process's environment. As for a
/// managed nested call, the family fills the model slot too.
pub(super) fn brokered_agent_identity(agent: Option<&str>, model: Option<&str>) -> Option<String> {
    normalize_agent_family_for_model(agent, model)
        .ok()
        .flatten()
        .or_else(|| agent.map(ToOwned::to_owned))
}

/// The audit role for a call a run's plugin broker executes, labelled from
/// the run's agent and model the way a managed envelope's identity is.
pub(super) fn brokered_role_label(agent: Option<&str>, model: Option<&str>) -> String {
    let family = brokered_agent_identity(agent, model);
    normalize_optional_attribution_label(family.as_deref().or(model), model)
        .unwrap_or_else(|| "agent".to_string())
}

fn read_input_identity(input: &Value) -> (Option<String>, Option<String>) {
    if let Value::Object(map) = input {
        let model = map
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        (None, model)
    } else {
        (None, None)
    }
}
