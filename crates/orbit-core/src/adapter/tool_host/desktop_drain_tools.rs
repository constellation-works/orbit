//! Desktop auto-drain translation. Authorization remains at the tool chokepoint;
//! scheduling, claims, stopping and settlement reuse the CLI/dashboard runtime.
use crate::application::job::DrainWorkerLimitRequest;
use crate::application::workflow::MAX_DRAIN_WINDOW_SECONDS;
use crate::{CompletionPolicy, DrainAdmissionsStopRequest, OrbitRuntime};
use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{optional_string, required_string};
use orbit_types::workflow::JobRunTrigger;
use serde_json::{Value, json};

pub(super) fn readiness(runtime: &OrbitRuntime) -> Result<Value, OrbitError> {
    // This is a bounded observational read, not a reconciliation tick.
    let mut value = runtime.workspace_auto_readiness(&[], None, 50, &[])?;
    value["controls_authorized"] = json!(true);
    Ok(value)
}

/// Inputs only `resize` accepts.
const RESIZE_ONLY_FIELDS: [&str; 3] = ["id", "if_revision", "reason"];

pub(super) fn control(
    runtime: &OrbitRuntime,
    input: Value,
    trigger: JobRunTrigger,
    actor: &str,
) -> Result<Value, OrbitError> {
    let action = required_string(&input, &["action"], "action")?;
    let claim = optional_string(&input, "claim_token")?;
    let workspace = input["workspace"].clone();
    if action != "stop" && input.get("force").is_some() {
        return Err(OrbitError::InvalidInput(format!(
            "`force` is a stop setting; {action} does not accept it"
        )));
    }
    if action != "resize"
        && let Some(field) = RESIZE_ONLY_FIELDS
            .iter()
            .find(|field| input.get(**field).is_some())
    {
        return Err(OrbitError::InvalidInput(format!(
            "`{field}` is a resize setting; {action} does not accept it"
        )));
    }
    let mut result = match action.as_str() {
        "start" => {
            // The drain refuses a longer window at its first step, so a start
            // past it would only submit a run that fails.
            let seconds = input
                .get("for_seconds")
                .and_then(Value::as_u64)
                .filter(|seconds| (1..=MAX_DRAIN_WINDOW_SECONDS).contains(seconds))
                .ok_or_else(|| {
                    OrbitError::InvalidInput(format!(
                        "for_seconds must be a whole number from 1 to {MAX_DRAIN_WINDOW_SECONDS} (24 hours)"
                    ))
                })?;
            let concurrency = input
                .get("concurrency")
                .map(|v| {
                    v.as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .filter(|n| *n > 0)
                        .ok_or_else(|| {
                            OrbitError::InvalidInput("concurrency must be a positive u32".into())
                        })
                })
                .transpose()?;
            let complete = input
                .get("complete")
                .map(|v| {
                    v.as_bool().ok_or_else(|| {
                        OrbitError::InvalidInput("complete must be a boolean".into())
                    })
                })
                .transpose()?
                .unwrap_or(false);
            let completion = if complete {
                CompletionPolicy::Done
            } else {
                CompletionPolicy::Review
            };
            let run = runtime.submit_workspace_auto_run(
                Some(seconds),
                concurrency,
                completion,
                &[],
                &Default::default(),
                Some("desktop"),
                claim.as_deref(),
                trigger,
            )?;
            let mut started = json!({"action":"start","run_id":run.run_id,"state":if run.queued {"queued"} else {"submitted"},
                "completion":completion.as_input_value(),"submitted_at":run.submitted_at});
            // [ORB-13901] The drain starts and holds its own waves while the
            // host is throttled; the caller learns why it admits nothing.
            if let Some(throttle) = runtime.admission_resource_throttle().throttle {
                started["warning"] = json!(throttle.hold_reason());
                started["resource_throttle"] = json!(throttle);
            }
            started
        }
        "stop" => {
            if ["for_seconds", "concurrency", "complete"]
                .iter()
                .any(|key| input.get(key).is_some())
            {
                return Err(OrbitError::InvalidInput(
                    "stop accepts no start settings".into(),
                ));
            }
            let force = match input.get("force") {
                None | Some(Value::Null) => false,
                Some(value) => value
                    .as_bool()
                    .ok_or_else(|| OrbitError::InvalidInput("force must be a boolean".into()))?,
            };
            let stopped = runtime.stop_workspace_auto_admissions(DrainAdmissionsStopRequest {
                actor: "desktop",
                source: "desktop",
                reason: Some(if force {
                    "Stopped with force from Orbit Control Center"
                } else {
                    "Stopped from Orbit Control Center"
                }),
                claim_token: claim.as_deref(),
                force,
            })?;
            // Preserve every unconfirmed stop, including local children,
            // when the request touches more than one coordinator.
            let unstopped = stopped
                .coordinators
                .iter()
                .flat_map(|change| {
                    change
                        .unstopped_leaves
                        .iter()
                        .map(|leaf| leaf.describe())
                        .chain(
                            change
                                .unstopped_children
                                .iter()
                                .map(|child| child.describe()),
                        )
                })
                .collect::<Vec<_>>();
            if !unstopped.is_empty() {
                return Err(OrbitError::Execution(format!(
                    "forced stop incomplete: {} run(s) could not be confirmed stopped: {}",
                    unstopped.len(),
                    unstopped.join("; ")
                )));
            }
            let coordinators = stopped.coordinators.iter().map(|change| json!({
                "run_id":change.run_id,"outcome":change.outcome,
                "remaining_children":change.remaining_children.iter().map(|child| json!({"run_id":child.run_id,"phase":child.phase})).collect::<Vec<_>>(),
                "forced_runs":change.forced_runs,
            })).collect::<Vec<_>>();
            json!({"action":"stop","outcome":stopped.outcome,"coordinators":coordinators,"pull_settlements":stopped.pull_settlements})
        }
        "resize" => resize(runtime, &input, claim.as_deref(), actor)?,
        _ => {
            return Err(OrbitError::InvalidInput(
                "action must be start, stop or resize".into(),
            ));
        }
    };
    result["workspace"] = workspace;
    result["schema_version"] = json!(1);
    Ok(result)
}

/// [ORB-11253] Move a live drain's worker ceiling without replacing its run:
/// the run ID, deadline, completion authorization and dispatched children
/// stay as they are, and a lower ceiling only stops new admissions. Without
/// an `id` it targets the workspace's one live auto or pull drain.
fn resize(
    runtime: &OrbitRuntime,
    input: &Value,
    claim: Option<&str>,
    actor: &str,
) -> Result<Value, OrbitError> {
    if ["for_seconds", "complete"]
        .iter()
        .any(|key| input.get(key).is_some())
    {
        return Err(OrbitError::InvalidInput(
            "resize accepts no start settings".into(),
        ));
    }
    let concurrency = optional_u32(input, "concurrency")?
        .ok_or_else(|| OrbitError::InvalidInput("resize requires `concurrency`".into()))?;
    let expected_revision = optional_u32(input, "if_revision")?;
    let reason = optional_string(input, "reason")?;
    let run_id = match optional_string(input, "id")? {
        Some(id) => id,
        None => runtime.active_drain_run_id()?,
    };
    let change = runtime.set_drain_worker_limit(DrainWorkerLimitRequest {
        run_id: &run_id,
        max_active_leaf_runs: concurrency,
        expected_revision,
        reason: reason.as_deref(),
        actor,
        source: "tool",
        claim_token: claim,
    })?;
    Ok(json!({
        "action": "resize",
        "run_id": change.run_id,
        "job_id": change.job_id,
        "outcome": change.outcome,
        "previous_concurrency": change.previous_max_active_leaf_runs,
        "concurrency": change.max_active_leaf_runs,
        "revision": change.revision,
    }))
}

fn optional_u32(input: &Value, field: &str) -> Result<Option<u32>, OrbitError> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!("`{field}` must be a non-negative integer"))
            }),
    }
}
