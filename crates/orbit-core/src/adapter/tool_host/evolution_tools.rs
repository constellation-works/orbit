use crate::OrbitRuntime;
use chrono::{DateTime, Utc};
use orbit_common::{
    OrbitError,
    governance::{evolution::EvolutionVerb, operation::OperationVerb},
};
use orbit_types::record::EvolutionState;
use serde_json::Value;

pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    verb: EvolutionVerb,
    input: Value,
) -> Result<Value, OrbitError> {
    let input = verb.spec().parse_input(input)?;
    match verb {
        EvolutionVerb::Add => {
            let event =
                serde_json::from_value(input.as_value()["event"].clone()).map_err(|error| {
                    OrbitError::InvalidInput(format!("invalid cost event: {error}"))
                })?;
            runtime.add_evolution_cost(
                event,
                input.optional_string("mandate")?,
                input.optional_string("cycle")?,
            )
        }
        EvolutionVerb::List => {
            let state = input
                .optional_string("state")?
                .map(|value| {
                    serde_json::from_value::<EvolutionState>(Value::String(value)).map_err(|_| {
                        OrbitError::InvalidInput(
                            "state must be open, proposed, declined or resolved".into(),
                        )
                    })
                })
                .transpose()?;
            let since = input
                .optional_string("since")?
                .map(|value| {
                    DateTime::parse_from_rfc3339(&value)
                        .map(|at| at.with_timezone(&Utc))
                        .map_err(|_| {
                            OrbitError::InvalidInput("since must be an RFC 3339 timestamp".into())
                        })
                })
                .transpose()?;
            runtime.list_evolution_records(state, since)
        }
        EvolutionVerb::Show => runtime.show_evolution_record(&input.required_string("id")?),
        EvolutionVerb::Decline => {
            let days = input
                .as_value()
                .get("days")
                .map(|value| {
                    value
                        .as_u64()
                        .and_then(|days| u32::try_from(days).ok())
                        .ok_or_else(|| {
                            OrbitError::InvalidInput("days must be a positive integer".into())
                        })
                })
                .transpose()?
                .unwrap_or(crate::application::evolution::EVOLUTION_DECLINE_SUPPRESS_DAYS);
            serde_json::to_value(
                runtime.decline_evolution_record(&input.required_string("id")?, days)?,
            )
            .map_err(|error| OrbitError::Execution(error.to_string()))
        }
    }
}
