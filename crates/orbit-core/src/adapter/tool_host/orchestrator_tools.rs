use crate::OrbitRuntime;
use crate::application::orchestrators::OrchestratorQuery;
use orbit_common::{
    OrbitError,
    governance::{operation::OperationVerb, orchestrator::OrchestratorVerb},
};
use serde_json::Value;

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: OrchestratorVerb,
    input: Value,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?;
    let query = OrchestratorQuery::parse(
        input.optional_string("window")?.as_deref(),
        input.optional_count("min_support")?,
    )?;
    match verb {
        OrchestratorVerb::Scorecard => runtime.orchestrator_scorecard(&query),
        OrchestratorVerb::Show => {
            runtime.orchestrator_detail(&query, &input.required_string("orchestrator")?)
        }
    }
}
