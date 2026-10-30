//! The `orbit.visual.*` tools: the implementer's mock self-check capture
//! and the operator's acceptance of a disputed visual finding.
use crate::OrbitRuntime;
use orbit_common::{
    OrbitError,
    governance::{operation::OperationVerb, visual::VisualVerb},
};
use serde_json::Value;

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: VisualVerb,
    input: Value,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?;
    match verb {
        VisualVerb::Capture => {
            let report = runtime.visual_capture(
                &input.required_string("id")?,
                input.optional_string("mock")?.as_deref(),
                input.optional_string("out")?.as_deref(),
            )?;
            serde_json::to_value(report)
                .map_err(|error| OrbitError::Execution(format!("visual capture report: {error}")))
        }
        VisualVerb::AcceptDispute => runtime.visual_accept_dispute(
            &input.required_string("id")?,
            &input.required_string("finding")?,
            &input.required_string("reason")?,
        ),
    }
}
