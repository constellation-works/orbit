//! The `orbit.agents.*` tools: one scan per call, sliced by role and selection.
use crate::OrbitRuntime;
use crate::application::agents::{AgentQuery, AgentRole};
use orbit_common::{
    OrbitError,
    governance::{agents::AgentsVerb, operation::OperationVerb},
};
use serde_json::Value;

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: AgentsVerb,
    input: Value,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?;
    let role = AgentRole::parse(&input.required_string("role")?)?;
    let query = AgentQuery::parse(
        input.optional_string("window")?.as_deref(),
        input.optional_count("min_support")?,
    )?;
    match verb {
        AgentsVerb::Scorecard => runtime.agent_role_scorecard(role, &query),
        AgentsVerb::Show => runtime.agent_role_detail(
            role,
            &query,
            &input.required_string("crew")?,
            input.optional_string("row_model")?.as_deref(),
        ),
    }
}
