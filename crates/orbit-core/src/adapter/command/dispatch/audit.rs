//! Audit correlation and agent identity: the trusted context and role label
//! a tool call is recorded under.

use orbit_common::OrbitError;
use orbit_tools::ReservationOwnerContext;
use orbit_types::identity::{
    normalize_agent_family_for_model, normalize_optional_attribution_label,
};
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

use crate::runtime::run_input::{
    managed_run_context_from_env, managed_run_context_run_id_from_env,
};

use super::execute::ToolEntryPoint;

/// Trusted audit-correlation fields at the MCP/CLI dispatch seam.
#[derive(Debug, Default, Clone)]
pub struct AuditContext {
    pub task_id: Option<String>,
    pub job_run_id: Option<String>,
    pub activity_id: Option<String>,
    pub step_index: Option<i64>,
}

pub(super) fn resolve_audit_context(
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
    fn env_str(name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    if entry_point == ToolEntryPoint::Mcp {
        // The MCP entry point never reads model-authored tool JSON for
        // correlation; `session_context` is retained in the signature because
        // it is the trusted envelope the caller must hand over to reach it.
        let _ = session_context;
        return trusted_mcp_audit_context();
    }

    AuditContext {
        task_id: input_str(input, "task_id").or_else(|| env_str("ORBIT_TASK_ID")),
        job_run_id: input_str(input, "job_run_id")
            .or_else(|| input_str(input, "run_id"))
            .or_else(|| env_str("ORBIT_RUN_ID")),
        activity_id: input_str(input, "activity_id").or_else(|| env_str("ORBIT_ACTIVITY_ID")),
        step_index: input
            .get("step_index")
            .and_then(Value::as_i64)
            .or_else(|| env_str("ORBIT_STEP_INDEX").and_then(|s| s.parse().ok())),
    }
}

/// Resolve MCP audit correlation exclusively from the managed process
/// envelope. Model-authored tool JSON and session audit metadata are not
/// correlation inputs.
pub fn trusted_mcp_audit_context() -> AuditContext {
    fn env_str(name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    if managed_run_context() {
        AuditContext {
            task_id: env_str("ORBIT_TASK_ID"),
            job_run_id: env_str("ORBIT_RUN_ID"),
            activity_id: env_str("ORBIT_ACTIVITY_ID"),
            step_index: env_str("ORBIT_STEP_INDEX").and_then(|value| value.parse().ok()),
        }
    } else {
        AuditContext::default()
    }
}

pub(super) fn reservation_owner_from_env() -> Option<ReservationOwnerContext> {
    managed_run_context_run_id_from_env().map(|owner_run_id| ReservationOwnerContext {
        owner_metadata_json: Some(
            serde_json::json!({
                "source": "orbit_cli",
            })
            .to_string(),
        ),
        owner_run_id,
    })
}

pub(super) fn managed_run_context() -> bool {
    managed_run_context_from_env()
}

fn read_agent_identity_from_env() -> (Option<String>, Option<String>) {
    let agent = std::env::var("ORBIT_AGENT_NAME")
        .ok()
        .filter(|s| !s.is_empty());
    let model = std::env::var("ORBIT_AGENT_MODEL")
        .ok()
        .filter(|s| !s.is_empty());
    (agent, model)
}

fn resolve_agent_identity(
    agent_override: Option<String>,
    model_override: Option<String>,
) -> Result<(Option<String>, Option<String>), OrbitError> {
    let (env_agent_name, env_model_name) = read_agent_identity_from_env();
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
    // Tool-call identity crosses a trust boundary: agent-supplied `model`
    // strings are telemetry at best and may be aliases. Persist the canonical
    // family in the model slot for tool dispatch so comparisons never depend
    // on self-reported model text.
    Ok((agent.clone(), agent))
}

pub(super) fn resolve_agent_identity_for_entry_point(
    entry_point: ToolEntryPoint,
    agent_override: Option<String>,
    model_override: Option<String>,
) -> Result<(Option<String>, Option<String>), OrbitError> {
    if entry_point == ToolEntryPoint::Mcp && !managed_run_context() {
        return Ok((None, None));
    }
    resolve_agent_identity(agent_override, model_override)
}

/// Resolve the audit `role` label for a tool invocation.
///
/// Runtime envelope identity (`ORBIT_AGENT_*`) is authoritative for agent
/// activities and overwrites any self-reported `model` field in tool JSON.
/// Manual CLI/MCP calls without an envelope keep the legacy input/flag
/// precedence.
pub fn audit_role_label(
    input: &Value,
    agent_override: Option<&str>,
    model_override: Option<&str>,
) -> String {
    let (input_agent, input_model) = read_input_identity(input);
    let env_agent = std::env::var("ORBIT_AGENT_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let env_model = std::env::var("ORBIT_AGENT_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let has_input_identity = input_agent.is_some() || input_model.is_some();
    let has_flag_identity = agent_override.is_some_and(|value| !value.trim().is_empty())
        || model_override.is_some_and(|value| !value.trim().is_empty());
    let has_env_identity = env_agent.is_some() || env_model.is_some();
    let (agent, model) = if has_env_identity && !has_flag_identity {
        let agent = normalize_agent_family_for_model(env_agent.as_deref(), env_model.as_deref())
            .ok()
            .flatten()
            .or(env_agent);
        (agent.clone(), agent)
    } else if has_input_identity {
        (input_agent, input_model)
    } else if has_flag_identity {
        (
            agent_override
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            model_override
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
        )
    } else {
        (env_agent, env_model)
    };
    let agent = normalize_agent_family_for_model(agent.as_deref(), model.as_deref())
        .ok()
        .flatten()
        .or(agent);

    normalize_optional_attribution_label(model.as_deref().or(agent.as_deref()), model.as_deref())
        .unwrap_or_else(|| "agent".to_string())
}

/// Resolve the audit role with the MCP trust boundary applied. Standalone MCP
/// calls are always `unverified`; an authenticated managed envelope may use
/// only its engine-provided identity and never caller JSON/flags.
pub fn audit_role_label_for_entry_point(
    input: &Value,
    agent_override: Option<&str>,
    model_override: Option<&str>,
    entry_point: ToolEntryPoint,
) -> String {
    if entry_point != ToolEntryPoint::Mcp {
        return audit_role_label(input, agent_override, model_override);
    }
    if !managed_run_context() {
        return "unverified".to_string();
    }

    let env_agent = std::env::var("ORBIT_AGENT_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty());
    let env_model = std::env::var("ORBIT_AGENT_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty());
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

fn read_input_identity(input: &Value) -> (Option<String>, Option<String>) {
    if let Value::Object(map) = input {
        let agent = map
            .get("agent")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let model = map
            .get("model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        (agent, model)
    } else {
        (None, None)
    }
}
