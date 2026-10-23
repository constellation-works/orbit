//! The deprecated `orbit.orchestrator.*` tools: the agents scorecard for the
//! orchestrators role, with a note naming the replacement. Removed in the
//! release after the agents tools ship.
use crate::OrbitRuntime;
use crate::application::agents::{AgentQuery, AgentRole};
use orbit_common::{
    OrbitError,
    governance::{operation::OperationVerb, orchestrator::OrchestratorVerb},
};
use serde_json::{Value, json};

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: OrchestratorVerb,
    input: Value,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?;
    let query = AgentQuery::parse(
        input.optional_string("window")?.as_deref(),
        input.optional_count("min_support")?,
    )?;
    let (mut payload, replacement) = match verb {
        OrchestratorVerb::Scorecard => (
            runtime.agent_role_scorecard(AgentRole::Orchestrators, &query)?,
            "`orbit agents scorecard --role orchestrators` or the `orbit.agents.scorecard` tool with role orchestrators",
        ),
        OrchestratorVerb::Show => (
            runtime.agent_role_detail(
                AgentRole::Orchestrators,
                &query,
                &input.required_string("orchestrator")?,
                input.optional_string("row_model")?.as_deref(),
            )?,
            "`orbit agents show --role orchestrators --crew <name>` or the `orbit.agents.show` tool with role orchestrators and the name as crew",
        ),
    };
    payload["deprecation"] = json!(format!(
        "the orchestrator scorecard is deprecated and is removed in the next release; use {replacement}"
    ));
    Ok(payload)
}
