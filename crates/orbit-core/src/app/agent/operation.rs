//! The agents registry flows: one scan per call, sliced by role and
//! selection, and the recorded CI failures by cause.

use orbit_common::OrbitError;
use orbit_common::governance::agents::AgentsVerb;
use orbit_common::governance::operation::OperationInput;
use orbit_types::tool::{McpTransport, ToolSessionContext};
use serde_json::Value;

use super::api::AgentDetailSurface;
use super::{AgentQuery, AgentRole, CiFailureQuery, api, ci_failure};
use crate::OrbitRuntime;
use crate::app::tool_call::Caller;

/// The surface a detail read names in its commands, from the caller's session.
fn agent_detail_surface(session: &ToolSessionContext) -> AgentDetailSurface {
    if session.origin_session_id.is_some() || session.transport == Some(McpTransport::SshMcp) {
        AgentDetailSurface::Mcp
    } else if session.transport.is_some() {
        AgentDetailSurface::ToolRun
    } else {
        AgentDetailSurface::Cli
    }
}

/// Run one agents verb. The tool entry checked `input` against the verb's
/// spec.
pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: AgentsVerb,
    input: &OperationInput,
    caller: &Caller,
) -> Result<Value, OrbitError> {
    match verb {
        AgentsVerb::CiFailures => {
            return ci_failure::ci_failure_records(
                runtime,
                &CiFailureQuery::parse(
                    input.optional_string("window")?.as_deref(),
                    input.optional_string("until")?.as_deref(),
                    input.optional_string("task")?.as_deref(),
                    input.optional_string("class")?.as_deref(),
                )?,
            );
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
            api::per_task_export(runtime, &query)
        }
        AgentsVerb::Scorecard if by_type => {
            if role != AgentRole::Implementers {
                return Err(OrbitError::InvalidInput(
                    "by_type is only supported for implementers".to_string(),
                ));
            }
            api::by_type_scorecard(runtime, &query)
        }
        AgentsVerb::Scorecard => api::agent_role_scorecard(runtime, role, &query),
        AgentsVerb::CiFailures => Err(OrbitError::Execution(
            "orbit.agents.ci_failures is answered before the role selection".to_string(),
        )),
        AgentsVerb::Show => api::agent_role_detail_with_effort(
            runtime,
            role,
            &query,
            &input.required_string("crew")?,
            input.optional_string("row_model")?.as_deref(),
            input.optional_string("row_effort")?.as_deref(),
            agent_detail_surface(&caller.session),
        ),
    }
}
