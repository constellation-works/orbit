//! A claimed leaf's read of its kept candidate's evidence findings through
//! its run's broker [ORB-18667].
//!
//! When an owner evidence run reports findings on a held candidate, the owner
//! writes every retained result's rule, location and data flow to the
//! system-written `review-evidence-findings.json` and queues the candidate's
//! repair. Its comment repeats only a bounded summary, and a claimed leaf that
//! resumes the candidate receives the claim's kept candidate without the
//! findings, so the results past that cut reach the leaf only from the
//! artifact. That artifact sits in the review gate's reserved namespace,
//! behind the reviewer's attempt scope ([`super::review`]).
//!
//! The broker carries this one read, and nothing else of the namespace, for
//! any activity but final recovery of the claimed leaf the worker binding
//! names while it holds its claim, named exactly as the canonical artifact.
//! The candidate binding comes from host records alone: the leaf's admission
//! receipt names the kept candidate the owner offered this claim to resume,
//! its held run and commit, and the owner's artifact must be the system's
//! findings on exactly that run and commit. A write of the artifact stays
//! refused by the reviewer's scope.

use orbit_common::OrbitError;
use orbit_types::tool::{ToolSessionContext, WorkerInvocation};
use orbit_types::workflow::{REVIEW_EVIDENCE_FINDINGS_ARTIFACT, ReviewEvidenceFindings};
use serde_json::{Map, Value, json};

use super::owner::{GET, accept_fields, require_claimed_task};
use super::review::{artifact_bytes, denied};
use crate::OrbitRuntime;
use crate::runtime::tool_exec::CapabilityEnforcement;

/// Whether a `review-*` call is the findings read this scope decides.
pub(super) fn is_findings_read(tool: &str, path: &str) -> bool {
    tool == GET && path == REVIEW_EVIDENCE_FINDINGS_ARTIFACT
}

/// Execute a claimed leaf's read of [`REVIEW_EVIDENCE_FINDINGS_ARTIFACT`] for
/// the claim [`super::owner`] derived. `path` is the request's
/// canonical artifact path; the request must also have spelled it that way.
pub(super) fn execute_findings_read(
    runtime: &OrbitRuntime,
    run: &orbit_engine::PluginBrokerRun,
    binding: &WorkerInvocation,
    object: &Map<String, Value>,
    path: &str,
    session: ToolSessionContext,
) -> Result<Value, OrbitError> {
    accept_fields(object, &["id", "path", "model"])?;
    require_claimed_task(object, binding)?;
    if object.get("path").and_then(Value::as_str) != Some(path) {
        return Err(denied(&format!(
            "findings_read_refused: name `{REVIEW_EVIDENCE_FINDINGS_ARTIFACT}` exactly"
        )));
    }
    let job_run_id = run
        .job_run_id
        .as_deref()
        .ok_or_else(|| denied("the run has no job-run authority"))?;
    let leaf = runtime
        .live_claimed_leaf(job_run_id)
        .map_err(|error| match error {
            OrbitError::PolicyDenied(reason) => denied(&reason),
            other => other,
        })?;
    let kept = leaf
        .admission
        .receipt
        .as_ref()
        .and_then(|receipt| receipt.task.as_ref())
        .filter(|task| task.id == leaf.claim.task_id)
        .and_then(|task| task.resume_candidate.as_ref());
    let Some((held_run, held_commit)) = kept
        .and_then(|candidate| Some((candidate.source_run_id.clone()?, candidate.head_sha.clone())))
    else {
        return Err(denied(
            "no_kept_candidate: this claim was admitted without a kept candidate to resume, so \
             it has no evidence findings to read",
        ));
    };
    runtime.authorize_tool_operation(GET, &session, CapabilityEnforcement::McpSessionOnly)?;
    let mut input = json!({"id": leaf.claim.task_id, "path": path});
    if let Some(model) = object.get("model") {
        input["model"] = model.clone();
    }
    let output = runtime.route_worker_tool(GET, input, session)?;
    if output.get("created_by").and_then(Value::as_str) != Some("system") {
        return Err(denied(&format!(
            "findings_unverified: the owner's `{REVIEW_EVIDENCE_FINDINGS_ARTIFACT}` was not \
             written by Orbit's system writer"
        )));
    }
    let bytes = artifact_bytes(&output, path)?;
    let bound = serde_json::from_slice::<ReviewEvidenceFindings>(&bytes).is_ok_and(|findings| {
        findings.schema_version == 1
            && findings.hold_run_id == held_run
            && findings.candidate.commit == held_commit
    });
    if !bound {
        return Err(denied(&format!(
            "findings_candidate_mismatch: the owner's `{REVIEW_EVIDENCE_FINDINGS_ARTIFACT}` is not \
             the findings on the kept candidate {held_commit} of run '{held_run}' this claim \
             resumes"
        )));
    }
    Ok(output)
}
