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
    let by_type = input.optional_bool("by_type")?.unwrap_or(false);
    let shadow = input.optional_bool("shadow")?.unwrap_or(false);
    let per_task = input.optional_bool("per_task")?.unwrap_or(false);
    match verb {
        AgentsVerb::Scorecard if per_task => {
            if role != AgentRole::Implementers {
                return Err(OrbitError::InvalidInput(
                    "per_task is only supported for implementers".to_string(),
                ));
            }
            if shadow || by_type {
                return Err(OrbitError::InvalidInput(
                    "per_task cannot be combined with by_type or shadow".to_string(),
                ));
            }
            runtime.agent_per_task_export(&query)
        }
        AgentsVerb::Scorecard if shadow => {
            if !by_type || role != AgentRole::Implementers {
                return Err(OrbitError::InvalidInput(
                    "shadow is only supported for implementers with by_type".to_string(),
                ));
            }
            runtime.agent_shadow_scorecard(&query)
        }
        AgentsVerb::Scorecard if by_type => {
            if role != AgentRole::Implementers {
                return Err(OrbitError::InvalidInput(
                    "by_type is only supported for implementers".to_string(),
                ));
            }
            runtime.agent_by_type_scorecard(&query)
        }
        AgentsVerb::Scorecard => runtime.agent_role_scorecard(role, &query),
        AgentsVerb::Show => runtime.agent_role_detail(
            role,
            &query,
            &input.required_string("crew")?,
            input.optional_string("row_model")?.as_deref(),
        ),
    }
}
