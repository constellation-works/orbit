//! The review projection existing task surfaces render [ORB-11333].
//!
//! Everything shown comes from durable evidence: the settled certificate the
//! gate wrote as a task artifact, the lineage ledger, and any landing
//! records. A task without a certificate simply has no review projection.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{ArtifactManifestFileV2, Task};
use orbit_types::workflow::{REVIEW_GATE_ARTIFACT, ReviewCertificate};
use serde_json::{Value, json};

use crate::OrbitRuntime;

/// The review gate evidence for one task, if a gate ever settled it.
///
/// The caller supplies the artifact manifest it already loaded with the
/// task, so an ungated task costs no further bundle read; only a listed
/// certificate is fetched.
pub fn task_review_projection(
    runtime: &OrbitRuntime,
    task: &Task,
    artifacts: &[ArtifactManifestFileV2],
) -> Result<Option<Value>, OrbitError> {
    if !artifacts
        .iter()
        .any(|file| file.path == REVIEW_GATE_ARTIFACT)
    {
        return Ok(None);
    }
    let Some(artifact) = runtime.get_task_artifact(&task.id, REVIEW_GATE_ARTIFACT)? else {
        return Ok(None);
    };
    let certificate: ReviewCertificate = match serde_json::from_slice(&artifact.content) {
        Ok(certificate) => certificate,
        Err(error) => {
            return Ok(Some(json!({
                "contract_version": null,
                "unreadable": error.to_string(),
                "artifact": REVIEW_GATE_ARTIFACT,
            })));
        }
    };
    let store = runtime.review_store()?;
    let ledger = store.review_ledger(&runtime.workspace_id()?, &certificate.lineage_key)?;
    let landings = store.review_landings(&certificate.attempt_id)?;
    let stale_reasons = landings
        .iter()
        .filter(|landing| !landing.covered)
        .filter_map(|landing| landing.reason.clone())
        .collect::<Vec<_>>();

    Ok(Some(json!({
        "contract_version": certificate.schema_version,
        "attempt_id": certificate.attempt_id,
        "lineage_key": certificate.lineage_key,
        "verdict": certificate.verdict.as_str(),
        "assurance": certificate.assurance.map(|assurance| assurance.as_str()),
        "passed": certificate.verdict.passed(),
        "escalation": certificate.escalation,
        "reviewer": certificate.reviewer,
        "base": certificate.base,
        "reviewed_candidate": certificate.reviewed_candidate,
        "final_candidate": certificate.final_candidate,
        "implementation_commits": certificate.implementation_commits,
        "repair_commits": certificate.repair_commits,
        "findings": certificate.findings,
        "validation": certificate.validation,
        "validation_complete": certificate.validation_complete,
        "task_meaning_digest": certificate.task_meaning_digest,
        "budget": certificate.budget,
        "consumed": certificate.consumed,
        "remaining": ledger.as_ref().map(|ledger| ledger.remaining_at(Utc::now())),
        "attempts": ledger.as_ref().map(|ledger| ledger.attempts.len()).unwrap_or(0),
        "landings": landings,
        "stale_reasons": stale_reasons,
        "issued_at": certificate.issued_at.to_rfc3339(),
        "artifact": REVIEW_GATE_ARTIFACT,
    })))
}
