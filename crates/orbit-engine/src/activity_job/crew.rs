//! Crew settings resolver (ADR-0330).
//!
//! A rendered activity input that carries `crew` selects that crew. Otherwise
//! the run input is used, so dispatch inherits the run's already-resolved crew.
//! The host returns the selected assignment and this module applies it over the
//! inline activity baseline field by field.

use orbit_types::identity::ReasoningEffort;
use orbit_types::workflow::activity_job::{AgentLoopSpec, Provider};
use serde_json::Value;

use crate::context::CrewConfig;

use super::dispatcher::DispatchError;
use crate::context::RuntimeHost;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAgentSettings {
    pub provider: Provider,
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// A run-scoped restriction on the crews this run may launch [ORB-11242]. The
/// key travels on the *run* input, so the restriction survives whichever input
/// selects the crew for a given activity.
const ALLOWED_CREWS_KEY: &str = "allowed_crews";

/// Resolve one crew assignment for an activity. Explicit activity input wins;
/// absent that, the run input preserves the run's resolved crew selection.
pub fn resolve_crew_settings(
    host: &dyn RuntimeHost,
    inline: &AgentLoopSpec,
    activity_input: &Value,
    run_input: &Value,
) -> Result<Option<ResolvedAgentSettings>, DispatchError> {
    let selected = if explicit_crew(activity_input).is_some() {
        activity_input
    } else {
        run_input
    };
    let input = with_run_crew_allowlist(selected, run_input);
    let config = host.agent_crew_config_for_input(&input)?;
    Ok(config
        .as_ref()
        .map(|config| resolve_from_config(config, inline)))
}

/// Carry the run's crew allowlist onto whichever input selects the crew.
///
/// [ORB-11242] An activity that names its own crew — including the
/// `workflow.system_crew` injection below — would otherwise be resolved from
/// an input that never saw the run's restriction, which is exactly how a
/// system override reaches a provider the operator excluded. The run input is
/// the authority and always overwrites, so an activity input cannot widen it.
fn with_run_crew_allowlist(selected: &Value, run_input: &Value) -> Value {
    let Some(allowed) = run_input.get(ALLOWED_CREWS_KEY) else {
        return selected.clone();
    };
    let mut input = selected.clone();
    if let Some(object) = input.as_object_mut() {
        object.insert(ALLOWED_CREWS_KEY.to_string(), allowed.clone());
    }
    input
}

/// Add the configured system crew to an activity input that explicitly opts
/// into the system route. The marker is asset data, while the crew is read
/// from the runtime host for each dispatch.
pub fn inject_system_crew_input(
    host: &dyn RuntimeHost,
    input: &Value,
) -> Result<Value, DispatchError> {
    if input.get("system_crew").and_then(Value::as_bool) != Some(true) {
        return Ok(input.clone());
    }
    let crew = host.system_crew_for_dispatch().ok_or_else(|| {
        DispatchError::JobValidation(
            "system activity requests `workflow.system_crew`, but this runtime host does not provide that configuration"
                .to_string(),
        )
    })?;
    let mut input = input.clone();
    let object = input.as_object_mut().ok_or_else(|| {
        DispatchError::JobValidation(
            "system activity requests `workflow.system_crew`, but its input must be an object"
                .to_string(),
        )
    })?;
    object.insert("crew".to_string(), Value::String(crew));
    object.insert(
        "crew_config_key".to_string(),
        Value::String("workflow.system_crew".to_string()),
    );
    Ok(input)
}

fn explicit_crew(input: &Value) -> Option<&str> {
    input
        .get("crew")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(crate) fn resolve_from_config(
    config: &CrewConfig,
    inline: &AgentLoopSpec,
) -> ResolvedAgentSettings {
    ResolvedAgentSettings {
        provider: config.provider.unwrap_or(inline.provider),
        model: config.model.clone().or_else(|| inline.model.clone()),
        reasoning_effort: config.reasoning_effort.or(inline.reasoning_effort),
    }
}

pub fn apply_resolved_settings(spec: &mut AgentLoopSpec, resolved: &ResolvedAgentSettings) {
    spec.provider = resolved.provider;
    spec.model = resolved.model.clone();
    spec.reasoning_effort = resolved.reasoning_effort;
}
