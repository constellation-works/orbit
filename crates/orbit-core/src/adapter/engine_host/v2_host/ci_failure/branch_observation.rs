//! Per-observation retention for task-branch CI failures.
//!
//! One owner being under an execution claim, or one write failing, must not
//! abort filing of the rest of the snapshot. A protecting claim queues the
//! receipt and reports it as deferred; settlement writes the artifact. The
//! store makes that decision against the current claim inside its boundary.

use orbit_common::security::release::sha256_hex;
use serde_json::{Value, json};

use crate::OrbitRuntime;

use super::evidence::bounded_error;

/// What one retention pass did. `observation_errors` are recorded and do not
/// fail the sweep; `deferred_attribution` is only the receipts that were queued.
pub(super) struct BranchRetention {
    pub attributed: Vec<Value>,
    pub deferred_attribution: Vec<Value>,
    pub observation_errors: Vec<Value>,
}

pub(super) fn note_branch_retention(audit: &mut Value, retention: &BranchRetention) {
    let Some(object) = audit.as_object_mut() else {
        return;
    };
    object.insert(
        "deferred_attribution".to_string(),
        json!(&retention.deferred_attribution),
    );
    object.insert(
        "branch_observation_errors".to_string(),
        json!(&retention.observation_errors),
    );
}

/// Retain each observation on its own. Never returns an error for one item.
pub(super) fn retain_branch_observations(
    runtime: &OrbitRuntime,
    observations: &[(String, Value)],
) -> BranchRetention {
    let mut attributed = Vec::new();
    let mut deferred_attribution = Vec::new();
    let mut observation_errors = Vec::new();
    if observations.is_empty() {
        return BranchRetention {
            attributed,
            deferred_attribution,
            observation_errors,
        };
    }
    for (owner, failure) in observations {
        let Some(content) = observation_content(failure) else {
            observation_errors.push(observation_error(
                owner,
                failure,
                "branch observation could not be encoded",
            ));
            continue;
        };
        let path = format!(
            "ci-branch-observations/{}.json",
            sha256_hex(content.as_bytes())
        );
        match runtime.get_task_artifact(owner, &path) {
            Ok(Some(_)) => {
                attributed.push(attributed_entry(owner, failure, &path));
                continue;
            }
            Ok(None) => {}
            Err(error) => {
                observation_errors.push(observation_error(owner, failure, &error.to_string()));
                continue;
            }
        }
        let outcome = runtime.record_deferred_branch_observation(
            &orbit_store::contracts::DeferredBranchObservation {
                schema_version: 1,
                task_id: owner.to_string(),
                run_id: failure.get("run_id").cloned().unwrap_or(Value::Null),
                job_id: failure.get("job_id").cloned().unwrap_or(Value::Null),
                artifact_path: path.clone(),
                content,
                claiming_run_id: None,
                applied: false,
            },
        );
        match outcome {
            Ok(orbit_store::contracts::BranchObservationOutcome::Retained) => {
                attributed.push(attributed_entry(owner, failure, &path));
            }
            Ok(orbit_store::contracts::BranchObservationOutcome::Deferred { .. }) => {
                deferred_attribution.push(json!({
                    "task_id": owner,
                    "run_id": failure.get("run_id").cloned().unwrap_or(Value::Null),
                    "reason": "claimed",
                }));
            }
            Err(error) => {
                observation_errors.push(observation_error(owner, failure, &error.to_string()));
            }
        }
    }
    BranchRetention {
        attributed,
        deferred_attribution,
        observation_errors,
    }
}

fn observation_content(failure: &Value) -> Option<String> {
    serde_json::to_string(&json!({
        "schema_version": 1,
        "kind": "task_branch_ci_failure",
        "failure": failure,
    }))
    .ok()
}

fn attributed_entry(owner: &str, failure: &Value, path: &str) -> Value {
    json!({
        "task_id": owner,
        "run_id": failure.get("run_id"),
        "job_id": failure.get("job_id"),
        "artifact": path,
    })
}

fn observation_error(owner: &str, failure: &Value, message: &str) -> Value {
    json!({
        "task_id": owner,
        "run_id": failure.get("run_id").cloned().unwrap_or(Value::Null),
        "job_id": failure.get("job_id").cloned().unwrap_or(Value::Null),
        "message": bounded_error(message),
    })
}
