//! Per-observation retention for task-branch CI failures.
//!
//! One owner being under an execution claim, or one write failing, must not
//! abort filing of the rest of the snapshot. A protecting claim queues the
//! receipt and reports it as deferred; settlement writes the artifact.

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::TaskArtifact;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskUpdateParams;

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
    let claims = match runtime.resolve_execution_claims() {
        Ok(claims) => Some(claims),
        Err(error) => {
            orbit_common::tracing::warn!(
                error = %error,
                "execution claims could not be resolved; branch observations fall back to the write"
            );
            None
        }
    };
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
        let protecting = claims.as_ref().and_then(|claims| {
            claims.iter().find(|inspection| {
                inspection.claim.task_id == *owner && inspection.claim.phase.protects_footprint()
            })
        });
        if let Some(inspection) = protecting {
            queue_deferred(
                runtime,
                DeferredReceipt {
                    owner,
                    failure,
                    path: &path,
                    content: &content,
                    claiming_run_id: Some(inspection.claim.run_context.run_id.clone()),
                },
                &mut deferred_attribution,
                &mut observation_errors,
            );
            continue;
        }
        match runtime.update_task(
            owner,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact::from_text(path.clone(), content.clone())],
                ..Default::default()
            },
        ) {
            Ok(_) => attributed.push(attributed_entry(owner, failure, &path)),
            Err(error) if is_claim_refusal(&error) => queue_deferred(
                runtime,
                DeferredReceipt {
                    owner,
                    failure,
                    path: &path,
                    content: &content,
                    claiming_run_id: claiming_run_from_refusal(&error.to_string()),
                },
                &mut deferred_attribution,
                &mut observation_errors,
            ),
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

struct DeferredReceipt<'a> {
    owner: &'a str,
    failure: &'a Value,
    path: &'a str,
    content: &'a str,
    claiming_run_id: Option<String>,
}

fn queue_deferred(
    runtime: &OrbitRuntime,
    receipt: DeferredReceipt<'_>,
    deferred: &mut Vec<Value>,
    errors: &mut Vec<Value>,
) {
    let DeferredReceipt {
        owner,
        failure,
        path,
        content,
        claiming_run_id,
    } = receipt;
    let queued = runtime.record_deferred_branch_observation(
        &orbit_store::contracts::DeferredBranchObservation {
            schema_version: 1,
            task_id: owner.to_string(),
            run_id: failure.get("run_id").cloned().unwrap_or(Value::Null),
            job_id: failure.get("job_id").cloned().unwrap_or(Value::Null),
            artifact_path: path.to_string(),
            content: content.to_string(),
            claiming_run_id,
            applied: false,
        },
    );
    match queued {
        Ok(()) => deferred.push(json!({
            "task_id": owner,
            "run_id": failure.get("run_id").cloned().unwrap_or(Value::Null),
            "reason": "claimed",
        })),
        Err(error) => errors.push(observation_error(owner, failure, &error.to_string())),
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

/// Prefix of the unscoped claim-write refusal. A claim that appears between
/// the resolve and the write is deferred the same way as one seen up front.
fn is_claim_refusal(error: &OrbitError) -> bool {
    error
        .to_string()
        .contains("active execution claim requires a claim-scoped mutation")
}

fn claiming_run_from_refusal(message: &str) -> Option<String> {
    let rest = message.split_once(", run ")?.1;
    let run = rest.split(')').next()?.trim();
    (!run.is_empty()).then(|| run.to_string())
}
