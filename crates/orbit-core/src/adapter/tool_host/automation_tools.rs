//! The `orbit.automation.*` tools: the read-only condition explain.
use crate::OrbitRuntime;
use orbit_common::{
    OrbitError,
    governance::{automation::AutomationVerb, operation::OperationVerb},
};
use serde_json::Value;

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: AutomationVerb,
    input: Value,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?;
    match verb {
        AutomationVerb::Explain => {
            let definition = input.required_string("definition")?;
            let subject = input.optional_string("subject")?;
            let report = runtime.explain_automation_condition(&definition, subject.as_deref())?;
            serde_json::to_value(report)
                .map_err(|error| OrbitError::Execution(format!("automation explain: {error}")))
        }
    }
}
