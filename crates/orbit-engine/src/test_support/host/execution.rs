//! Shared fixture answers for scripts, deferral probes, activity tools and crews.

use serde_json::Value;

use crate::context::dispatch_contract::DispatchError;
use crate::{CrewConfig, ResolvedActivityTools, ScriptStepRequest, ScriptStepRun};
use orbit_types::workflow::{DeferralProbe, NamespaceProbe};

/// The `run_script_step` answer of a host that runs no scripts: a refusal
/// for the unavailable sandbox that records the requested script.
pub fn refuse_script_step(request: &ScriptStepRequest) -> ScriptStepRun {
    ScriptStepRun::Refused {
        code: orbit_types::workflow::activity_job::script_refusal::SANDBOX_UNAVAILABLE,
        message: "this host runs no script steps".to_string(),
        record: Box::new(orbit_types::workflow::activity_job::ScriptExecutionRecord {
            script: request.script.clone(),
            outcome: "refused".to_string(),
            refusal_code: Some(
                orbit_types::workflow::activity_job::script_refusal::SANDBOX_UNAVAILABLE
                    .to_string(),
            ),
            ..Default::default()
        }),
    }
}

/// The `run_deferral_probe` answer of a host that cannot run probes: exit
/// code -1 with a message that says why the probe did not run.
pub fn unavailable_deferral_probe(probe: &DeferralProbe) -> NamespaceProbe {
    NamespaceProbe {
        command: probe.command.to_string(),
        exit_code: -1,
        output: "the probe could not run: this host does not support deferral probes".to_string(),
    }
}

/// The `resolve_activity_tools` answer of a host without tool resolution: the
/// baseline tools are effective, and nothing is requested, granted or
/// suppressed.
pub fn baseline_activity_tools(baseline_tools: &[String]) -> ResolvedActivityTools {
    ResolvedActivityTools {
        requested_tools: Vec::new(),
        effective_tools: baseline_tools.to_vec(),
        omitted_requirement_notes: Vec::new(),
        granted_tools: Vec::new(),
        resident_tools: Vec::new(),
        suppressed_tools: Vec::new(),
    }
}

/// Overlays a crew's entries from the process environment onto an agent
/// environment the host already built. The caller dispatches the host's
/// `agent_subprocess_environment` first, so an override applies before this
/// overlay.
pub fn overlay_crew_env(
    parent_env: &orbit_common::Env,
    mut env: Vec<(String, String)>,
    crew_env: &[orbit_types::identity::CrewEnvVar],
) -> Vec<(String, String)> {
    orbit_common::security::child_env::apply_crew_env_from(
        parent_env,
        &mut env,
        &parent_env
            .utf8_vars()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect::<Vec<_>>(),
        crew_env,
    );
    env
}

/// The crew named by an activity input, which this host cannot resolve: an
/// explicit crew name is refused, and no crew name is `None`.
pub fn validate_input_crew(input: &Value) -> Result<Option<CrewConfig>, DispatchError> {
    if let Some(crew) = input
        .get("crew")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Err(DispatchError::JobValidation(format!(
            "explicit activity crew '{crew}' cannot be resolved by this runtime host"
        )));
    }
    Ok(None)
}
