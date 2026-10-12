use orbit_engine::{CrewConfig, DispatchError};
use orbit_types::task::TaskComplexity;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::job::crew_pools::{
    complexity_crews_key, complexity_of_crews_key, random_crew_ticket,
};

/// The retired `workflow.system_crew` activity route [ORB-15663]. A run
/// admitted before the lane was retired, or a job or activity a user edited,
/// may still carry it.
const RETIRED_SYSTEM_CREW_KEY: &str = "workflow.system_crew";

/// The pool a retired `system` route draws from: medium, the tier an
/// unassessed task is verified on, because the work that used the route
/// (recovery, the pilot) is not known to be low.
const RETIRED_SYSTEM_CREW_COMPLEXITY: TaskComplexity = TaskComplexity::Medium;

pub(super) fn agent_crew_config_for_input(
    runtime: &OrbitRuntime,
    input: &serde_json::Value,
) -> Result<Option<CrewConfig>, DispatchError> {
    let explicit = input
        .get("crew")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut config_key = input
        .get("crew_config_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    // [ORB-15663] The retired system route, and an explicit `crew: system`
    // this host does not define, draw from a complexity pool instead.
    let retired_route = config_key.as_deref() == Some(RETIRED_SYSTEM_CREW_KEY)
        || (config_key.is_none()
            && explicit.is_some_and(|crew| {
                orbit_config::names_retired_system_crew(crew, runtime.context.settings().crews())
            }));
    if retired_route {
        tracing::warn!(
            pool = %complexity_crews_key(RETIRED_SYSTEM_CREW_COMPLEXITY),
            "{}",
            orbit_config::RETIRED_SYSTEM_CREW_WARNING
        );
        config_key = Some(complexity_crews_key(RETIRED_SYSTEM_CREW_COMPLEXITY));
    }
    let config_key = config_key.as_deref();
    let crew = match config_key {
        Some(orbit_types::workflow::FINAL_RECOVERY_CREWS_KEY) => runtime
            .final_recovery_crew(input, &mut random_crew_ticket)
            .map_err(|error| {
                DispatchError::JobValidation(format!(
                    "activity crew drawn from `workflow.final_recovery_crews` cannot be resolved \
                     or used: {error}"
                ))
            })?,
        Some(key) => {
            let complexity = complexity_of_crews_key(key).ok_or_else(|| {
                DispatchError::JobValidation(format!(
                    "activity crew names unsupported configuration key `{key}`"
                ))
            })?;
            runtime
                .complexity_pool_activity_crew(input, complexity, &mut random_crew_ticket)
                .map_err(|error| {
                    DispatchError::JobValidation(format!(
                        "activity crew drawn from `{key}` cannot be resolved or used: {error}"
                    ))
                })?
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
    // A pool draw, an explicit activity crew, and the run's own crew
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
