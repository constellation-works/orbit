//! Desktop auto-drain translation. Authorization remains at the tool chokepoint;
//! scheduling, claims, stopping and settlement reuse the CLI/dashboard runtime.
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

pub(super) fn control(
    runtime: &OrbitRuntime,
    input: Value,
    trigger: JobRunTrigger,
) -> Result<Value, OrbitError> {
    let action = required_string(&input, &["action"], "action")?;
    let claim = optional_string(&input, "claim_token")?;
    let workspace = input["workspace"].clone();
    let mut result = match action.as_str() {
        "start" => {
            let seconds = input
                .get("for_seconds")
                .and_then(Value::as_u64)
                .filter(|seconds| (1..=604_800).contains(seconds))
                .ok_or_else(|| {
                    OrbitError::InvalidInput(
                        "for_seconds must be a whole number from 1 to 604800 (seven days)".into(),
                    )
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
            json!({"action":"start","run_id":run.run_id,"state":if run.queued {"queued"} else {"submitted"},
                "completion":completion.as_input_value(),"submitted_at":run.submitted_at})
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
            let stopped = runtime.stop_workspace_auto_admissions(DrainAdmissionsStopRequest {
                actor: "desktop",
                source: "desktop",
                reason: Some("Stopped from Orbit Control Center"),
                claim_token: claim.as_deref(),
            })?;
            let coordinators = stopped.coordinators.iter().map(|change| json!({
                "run_id":change.run_id,"outcome":change.outcome,
                "remaining_children":change.remaining_children.iter().map(|child| json!({"run_id":child.run_id,"phase":child.phase})).collect::<Vec<_>>()
            })).collect::<Vec<_>>();
            json!({"action":"stop","outcome":stopped.outcome,"coordinators":coordinators,"pull_settlements":stopped.pull_settlements})
        }
        _ => {
            return Err(OrbitError::InvalidInput(
                "action must be start or stop".into(),
            ));
        }
    };
    result["workspace"] = workspace;
    result["schema_version"] = json!(1);
    Ok(result)
}
