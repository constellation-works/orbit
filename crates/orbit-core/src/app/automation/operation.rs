//! The `orbit.automation.*` flows: the read-only condition and job explain,
//! and the operator's audited clear-stall.

use orbit_common::OrbitError;
use orbit_common::governance::automation::AutomationVerb;
use orbit_common::governance::operation::OperationInput;
use serde_json::Value;

use crate::OrbitRuntime;

/// Run one automation verb. The tool entry checked `input` against the
/// verb's spec.
pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: AutomationVerb,
    input: &OperationInput,
) -> Result<Value, OrbitError> {
    match verb {
        AutomationVerb::Explain => {
            let definition = input.required_string("definition")?;
            let subject = input.optional_string("subject")?;
            let report = runtime.explain_automation(&definition, subject.as_deref())?;
            serde_json::to_value(report)
                .map_err(|error| OrbitError::Execution(format!("automation explain: {error}")))
        }
        AutomationVerb::ClearStall => {
            let definition = input.required_string("definition")?;
            let reason = input.required_string("reason")?;
            let cleared = runtime.clear_condition_stall(&definition, &reason)?;
            serde_json::to_value(cleared)
                .map_err(|error| OrbitError::Execution(format!("automation clear-stall: {error}")))
        }
    }
}
