//! `orbit.resident.*`: translate canonical tool input into the resident use
//! cases. Whether a widening applies or becomes a grant request is the
//! application layer's decision, made from the session passed through here.

use orbit_common::OrbitError;
use orbit_common::governance::operation::{OperationInput, OperationVerb};
use orbit_common::governance::resident::ResidentVerb;
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::resident::{
    ResidentChannelCaller, ResidentControlAction, ResidentControlParams, ResidentGrantCaller,
    ResidentRelayParams, resident_grant_params_from_input,
};

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    session_context: &ToolSessionContext,
    verb: ResidentVerb,
    input: Value,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?;
    match verb {
        ResidentVerb::Grant => {
            let caller = ResidentGrantCaller {
                session: session_context.clone(),
                agent: agent.map(ToOwned::to_owned),
                model: input
                    .optional_string("model")?
                    .or_else(|| model.map(ToOwned::to_owned)),
                orchestrator: input.optional_string("orchestrator")?,
            };
            let outcome = runtime
                .grant_resident_authority(&caller, resident_grant_params_from_input(&input)?)?;
            serde_json::to_value(outcome)
                .map_err(|error| OrbitError::Execution(format!("serialize grant: {error}")))
        }
        ResidentVerb::Relay => runtime.relay_to_resident(
            &channel_caller(&input, agent, model)?,
            ResidentRelayParams {
                task_id: input.required_string("task_id")?,
                quote: input.optional_raw_string("quote")?.unwrap_or_default(),
                source: input.as_value()["source"].clone(),
                request_id: input.optional_string("request_id")?,
                reverses: input.optional_string("reverses")?,
            },
        ),
        ResidentVerb::Control => {
            let name = input.required_string("action")?;
            let action = ResidentControlAction::parse(&name).ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "unknown action `{name}`; actions are pause, resume, stop, wake, takeover and release"
                ))
            })?;
            runtime.control_resident(
                &channel_caller(&input, agent, model)?,
                ResidentControlParams {
                    task_id: input.required_string("task_id")?,
                    action,
                    reason: input.required_string("reason")?,
                    status_request: input.optional_bool("status_request")?.unwrap_or(false),
                    by_session: input.optional_string("by_session")?,
                },
            )
        }
        ResidentVerb::Checkpoint => runtime.append_resident_checkpoint(
            &input.required_string("task_id")?,
            input.as_value()["record"].clone(),
            agent.map(ToOwned::to_owned),
            input
                .optional_string("model")?
                .or_else(|| model.map(ToOwned::to_owned)),
        ),
        ResidentVerb::Digest => {
            let top = input
                .as_value()
                .get("top")
                .map(|value| {
                    value
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .ok_or_else(|| {
                            OrbitError::InvalidInput("top must be an integer from 1 to 100".into())
                        })
                })
                .transpose()?
                .unwrap_or(5);
            runtime.resident_digest(input.optional_string("since")?.as_deref(), top)
        }
        ResidentVerb::Metrics => runtime.resident_metrics(
            input.optional_string("mandate")?.as_deref(),
            input.optional_string("since")?.as_deref(),
            input.optional_string("baseline")?.as_deref(),
        ),
        ResidentVerb::Show => runtime.show_resident_mandate(&input.required_string("task_id")?),
        ResidentVerb::List => {
            runtime.list_resident_mandates(input.optional_bool("include_ended")?.unwrap_or(false))
        }
    }
}

fn channel_caller(
    input: &OperationInput,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<ResidentChannelCaller, OrbitError> {
    Ok(ResidentChannelCaller {
        agent: agent.map(ToOwned::to_owned),
        model: input
            .optional_string("model")?
            .or_else(|| model.map(ToOwned::to_owned)),
        orchestrator: input.optional_string("orchestrator")?,
    })
}
