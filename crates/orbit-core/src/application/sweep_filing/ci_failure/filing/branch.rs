//! Landing-branch and task-branch failures: which current failures belong to a
//! landing checkout, which belong to a task branch's owner, and the typed
//! compiler observations retained on an open owner.

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_engine::ci_run_event::is_branch_event;
use orbit_store::contracts::BranchObservationOutcome;
use orbit_types::task::is_valid_orb_task_id;
use orbit_types::workflow::{ConventionTemplate, DEFAULT_BRANCH_PREFIX};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::sweep_filing::bounded_error;

use super::super::branch_observation::{claimed_entry, retain_owner_receipt};
use super::super::cluster::FailureCluster;
use super::super::fields::value_string;
use super::CI_FAILURE_KEY_TAG_PREFIX;

/// Current failures split by checkout: the landing failures left to file, the
/// task-branch failures retained on their owner, and the branch failures
/// with no owner.
pub(super) struct BranchPartition {
    pub(super) complete: Vec<Value>,
    pub(super) branch_observations: Vec<(String, Value)>,
    pub(super) excluded_branch_failures: Vec<Value>,
}

/// An owner lookup that fails is deferred, so `deferred` gains one entry per
/// such failure.
pub(super) fn partition_branch_failures(
    runtime: &OrbitRuntime,
    evidence: &Value,
    complete: Vec<Value>,
    deferred: &mut Vec<Value>,
) -> BranchPartition {
    // Recheck old/replayed snapshots too: ref_kind and the PR/event head alone
    // do not prove the failing checkout belongs to a landing branch.
    let (complete, branch_failures): (Vec<_>, Vec<_>) = complete
        .into_iter()
        .partition(|failure| landing_checkout(evidence, failure));
    let mut branch_observations = Vec::new();
    let mut excluded_branch_failures = Vec::new();
    for failure in branch_failures {
        let Some(owner) = task_branch_owner(
            &runtime
                .context
                .settings()
                .pr_settings()
                .conventions
                .branch_name,
            &failure,
        ) else {
            excluded_branch_failures.push(failure);
            continue;
        };
        match runtime.get_task(&owner) {
            Ok(_) => branch_observations.push((owner, failure)),
            Err(error) => deferred.push(json!({
                "run_id": failure.get("run_id"), "job_id": failure.get("job_id"),
                "head_branch": failure.get("head_branch"), "retryable": true,
                "reasons": [{
                    "stage": "registration", "operation": "task_branch_owner",
                    "run_id": failure.get("run_id"), "job_id": failure.get("job_id"),
                    "retryable": true, "message": bounded_error(&error.to_string()),
                }],
            })),
        }
    }
    BranchPartition {
        complete,
        branch_observations,
        excluded_branch_failures,
    }
}

/// Branch-event runs (push, schedule or dispatch) may describe an older
/// landing commit; PR/queue checkouts must match a currently observed landing
/// tip. Never substitute the PR's source SHA.
fn landing_checkout(evidence: &Value, failure: &Value) -> bool {
    let Some(checkout) = failure["actual_checkout_shas"]
        .as_array()
        .filter(|shas| shas.len() == 1)
        .and_then(|shas| shas[0].as_str())
        .filter(|sha| !sha.is_empty())
    else {
        return false;
    };
    evidence["heads"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|head| {
            matches!(head["kind"].as_str(), Some("integration" | "release"))
                && (head["current_head_sha"].as_str() == Some(checkout)
                    || (failure["event"].as_str().is_some_and(is_branch_event)
                        && head["branch"].as_str().is_some()
                        && head["branch"] == failure["head_branch"]
                        && failure["event_reported_head_sha"].as_str() == Some(checkout)))
        })
}

/// The task a failing run's head branch belongs to, read through the
/// workspace's branch template (`orbit/<TASK-ID>-…` by default).
fn task_branch_owner(branch_template: &ConventionTemplate, failure: &Value) -> Option<String> {
    let branch = failure["head_branch"].as_str()?;
    branch_template.branch_task_id(branch, DEFAULT_BRANCH_PREFIX, &is_valid_orb_task_id)
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct CiCompilerObservation {
    run_id: String,
    run_url: String,
    checkout: String,
    failure_key: String,
}

/// Retain a typed receipt with each comment, backfilling either half a legacy
/// or partial write left out. Returns one `deferred_attribution` entry per
/// observation a protecting claim holds until it settles.
pub(super) fn retain_compiler_observations(
    runtime: &OrbitRuntime,
    owner: &str,
    cluster: &FailureCluster,
) -> Result<Vec<Value>, OrbitError> {
    let mut deferred = Vec::new();
    for run in &cluster.runs {
        let observation = CiCompilerObservation {
            run_id: value_string(run, "run_id"),
            run_url: value_string(run, "url"),
            checkout: cluster.tested_commit.clone(),
            failure_key: cluster.failure_key.clone(),
        };
        let content = serde_json::to_string(&observation)
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        let path = format!(
            "ci-compiler-observations/{}.json",
            sha256_hex(content.as_bytes())
        );
        let message = format!(
            "CI compiler observation: run `{}` {}; checkout `{}`; exact failure key `{CI_FAILURE_KEY_TAG_PREFIX}{}`.",
            observation.run_id, observation.run_url, observation.checkout, observation.failure_key,
        );
        let already_commented = runtime
            .get_task_comments(owner)?
            .iter()
            .any(|comment| comment.message == message);
        let artifact = runtime.get_task_artifact(owner, &path)?;
        if let Some(artifact) = &artifact {
            let stored: CiCompilerObservation = serde_json::from_slice(&artifact.content)
                .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
            if stored != observation {
                return Err(OrbitError::InvalidInput(
                    "compiler observation receipt does not match its identity".into(),
                ));
            }
        }
        // The receipt and the comment land together, but older writes may
        // have left only one of them.
        if artifact.is_some() && already_commented {
            continue;
        }
        let comment = (!already_commented).then_some(message);
        let outcome = retain_owner_receipt(runtime, owner, run, path.clone(), content, comment)?;
        if matches!(outcome, BranchObservationOutcome::Deferred { .. }) {
            deferred.push(claimed_entry(owner, run, &path));
        }
    }
    Ok(deferred)
}
