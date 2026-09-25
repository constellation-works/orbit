use orbit_engine::{CrewConfig, DispatchError};
use serde_json::Value;

use crate::OrbitRuntime;

pub(super) fn agent_crew_config_for_input(
    runtime: &OrbitRuntime,
    input: &serde_json::Value,
) -> Result<Option<CrewConfig>, DispatchError> {
    let explicit = input
        .get("crew")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let config_key = input
        .get("crew_config_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let crew = match config_key {
        Some("workflow.system_crew") => runtime
            .resolve_crew_for_task(Some(runtime.context.settings().system_crew()), None)
            .map_err(|error| {
                DispatchError::JobValidation(format!(
                    "activity crew configured by `workflow.system_crew` cannot be resolved or used: {error}"
                ))
            })?,
        Some(other) => {
            return Err(DispatchError::JobValidation(format!(
                "activity crew names unsupported configuration key `{other}`"
            )));
        }
        None => match explicit {
            Some(crew_name) => runtime
                .resolve_crew_for_task(Some(crew_name), None)
                .map_err(|error| {
                    DispatchError::JobValidation(format!(
                        "explicit activity crew `{crew_name}` cannot be resolved or used: {error}"
                    ))
                })?,
            None => runtime.resolve_crew_for_run_input(input).map_err(|error| {
                DispatchError::JobValidation(format!(
                    "run crew cannot be resolved or used for activity dispatch: {error}"
                ))
            })?,
        },
    };
    // [ORB-11242] The last gate before a provider process is launched, and
    // the only one that sees the crew *after* alias and default resolution.
    // A system override, an explicit activity crew, and the run's own crew
    // all arrive here, so one check covers every route into a provider.
    let allowlist = runtime.crew_allowlist_from_input(input).map_err(|error| {
        DispatchError::JobValidation(format!(
            "run crew allowlist cannot be resolved for activity dispatch: {error}"
        ))
    })?;
    let origin = match config_key {
        Some(key) => format!("`{key}`"),
        None if explicit.is_some() => "an explicit activity crew".to_string(),
        None => "this run's crew".to_string(),
    };
    crate::runtime::engine::crew::enforce_crew_allowlist(allowlist.as_ref(), &crew, &origin)
        .map_err(|error| DispatchError::JobValidation(error.to_string()))?;
    Ok(Some(
        crate::runtime::engine::environment_host::typed_crew_config_from_assignment(
            &crew.assignment,
        ),
    ))
}
