//! Handler table for the workspace-claim operation registry.

use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationVerb;
use orbit_common::governance::workspace_claim::WorkspaceClaimVerb;
use serde_json::Value;

use crate::OrbitRuntime;

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: WorkspaceClaimVerb,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?.into_value();
    match verb {
        WorkspaceClaimVerb::Acquire => {
            crate::runtime::workspace::claim::acquire(runtime, input, agent, model)
        }
        WorkspaceClaimVerb::Release => {
            crate::runtime::workspace::claim::release(runtime, input, agent, model)
        }
        WorkspaceClaimVerb::Show => crate::runtime::workspace::claim::show(runtime),
    }
}
