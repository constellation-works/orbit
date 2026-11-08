//! The `orbit.agents.*` tools: one scan per call, sliced by role and selection,
//! and the recorded CI failures by cause.
use crate::OrbitRuntime;
use crate::application::agents::{AgentDetailSurface, AgentQuery, AgentRole, CiFailureQuery};
use orbit_common::{
    OrbitError,
    governance::{agents::AgentsVerb, operation::OperationVerb},
};
use serde_json::Value;

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: AgentsVerb,
    input: Value,
    surface: AgentDetailSurface,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?;
    match verb {
        AgentsVerb::CiFailures => {
            return runtime.ci_failure_records(&CiFailureQuery::parse(
                input.optional_string("window")?.as_deref(),
                input.optional_string("until")?.as_deref(),
                input.optional_string("task")?.as_deref(),
                input.optional_string("class")?.as_deref(),
            )?);
        }
        AgentsVerb::Scorecard | AgentsVerb::Show => {}
    }
    let role = AgentRole::parse(&input.required_string("role")?)?;
    let query = AgentQuery::parse(
        input.optional_string("window")?.as_deref(),
        input.optional_count("min_support")?,
    )?
    .with_execution(input.optional_string("execution")?.as_deref())?
    .with_until(input.optional_string("until")?.as_deref())?;
    let by_type = input.optional_bool("by_type")?.unwrap_or(false);
    let per_task = input.optional_bool("per_task")?.unwrap_or(false);
    match verb {
        AgentsVerb::Scorecard if per_task => {
            if role != AgentRole::Implementers {
                return Err(OrbitError::InvalidInput(
                    "per_task is only supported for implementers".to_string(),
                ));
            }
            if by_type {
                return Err(OrbitError::InvalidInput(
                    "per_task cannot be combined with by_type".to_string(),
                ));
            }
            runtime.agent_per_task_export(&query)
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
        AgentsVerb::CiFailures => Err(OrbitError::Execution(
            "orbit.agents.ci_failures is answered before the role selection".to_string(),
        )),
        AgentsVerb::Show => runtime.agent_role_detail_for_surface(
            role,
            &query,
            &input.required_string("crew")?,
            input.optional_string("row_model")?.as_deref(),
            surface,
        ),
    }
}
