use crate::OrbitRuntime;
use crate::application::task::decision_request_json;
use orbit_common::OrbitError;
use orbit_common::governance::decision::DecisionVerb;
use orbit_common::governance::operation::OperationVerb;
use orbit_types::task::DecisionGate;
use serde_json::{Value, json};

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: DecisionVerb,
    input: Value,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?;
    match verb {
        DecisionVerb::List => {
            let gate = input
                .optional_string("gate")?
                .map(|name| {
                    DecisionGate::from_name(&name).ok_or_else(|| {
                        OrbitError::InvalidInput(format!(
                            "unknown decision gate `{name}`; expected block_triage, pilot_admission, proposed_approval or review_approval"
                        ))
                    })
                })
                .transpose()?;
            let task_id = input.optional_string("task_id")?;
            let include_closed = input.optional_bool("include_closed")?.unwrap_or(false);
            let requests =
                runtime.list_decision_requests(gate, task_id.as_deref(), include_closed)?;
            Ok(json!({
                "requests": requests.iter().map(decision_request_json).collect::<Vec<_>>(),
                "count": requests.len(),
            }))
        }
        DecisionVerb::Show => {
            let id = input.required_string("id")?;
            let request = runtime.get_decision_request(&id)?.ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "request_not_found: no decision request `{id}` in this workspace"
                ))
            })?;
            Ok(decision_request_json(&request))
        }
        // The tool entry refuses this verb before dispatch and registers it
        // inactive; refusing here too keeps the invariant in core.
        DecisionVerb::Answer => Err(OrbitError::PolicyDenied(
            "answering a decision request is the operator's: use `orbit decision answer` or the dashboard".into(),
        )),
    }
}
