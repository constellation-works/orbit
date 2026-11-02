//! `orbit.resident.*`: translate canonical tool input into the resident use
//! cases. Whether a widening applies or becomes a grant request is the
//! application layer's decision, made from the session passed through here.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::governance::operation::{OperationInput, OperationVerb};
use orbit_common::governance::resident::ResidentVerb;
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::ResidentRight;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::resident::{ResidentGrantCaller, ResidentGrantParams};

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
            let outcome = runtime.grant_resident_authority(&caller, grant_params(&input)?)?;
            serde_json::to_value(outcome)
                .map_err(|error| OrbitError::Execution(format!("serialize grant: {error}")))
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

fn rights(input: &OperationInput, field: &str) -> Result<Vec<ResidentRight>, OrbitError> {
    input
        .optional_string_list(field)?
        .unwrap_or_default()
        .iter()
        .map(|name| {
            ResidentRight::parse(name).ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "unknown right `{name}` in `{field}`; rights are {}",
                    ResidentRight::NAMES
                ))
            })
        })
        .collect()
}

fn grant_params(input: &OperationInput) -> Result<ResidentGrantParams, OrbitError> {
    let deadline = input
        .optional_string("deadline")?
        .map(|value| {
            DateTime::parse_from_rfc3339(&value)
                .map(|at| at.with_timezone(&Utc))
                .map_err(|_| {
                    OrbitError::InvalidInput("`deadline` must be an RFC 3339 timestamp".into())
                })
        })
        .transpose()?;
    Ok(ResidentGrantParams {
        task_id: input.required_string("task_id")?,
        add: rights(input, "add")?,
        workspaces: input
            .optional_string_list("workspaces")?
            .unwrap_or_default(),
        remove: rights(input, "remove")?,
        drop_workspaces: input
            .optional_string_list("drop_workspaces")?
            .unwrap_or_default(),
        deadline,
        quote: input.optional_raw_string("quote")?,
    })
}
