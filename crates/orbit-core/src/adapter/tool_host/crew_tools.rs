//! The `orbit.crew.*` tools: the read-only crew ladder report.
use crate::OrbitRuntime;
use orbit_common::{
    OrbitError,
    governance::{crew::CrewVerb, operation::OperationVerb},
};
use serde_json::Value;

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: CrewVerb,
    input: Value,
) -> Result<Value, OrbitError> {
    verb.spec().parse_input(input)?;
    match verb {
        CrewVerb::Ladder => serde_json::to_value(runtime.crew_ladder_report()?)
            .map_err(|error| OrbitError::Execution(format!("crew ladder report: {error}"))),
    }
}
