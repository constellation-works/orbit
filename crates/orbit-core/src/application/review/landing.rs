//! Verify how a reviewed candidate actually landed after a managed merge
//! [ORB-11333].
//!
//! Completion knows the PR it merged, the head it pinned, and the merge
//! commit the provider reported. Core fetches the integration branch, reads
//! the landed objects, and lets the shared rule classify the mapping. An
//! unverifiable landing is recorded as uncovered; nothing is fabricated.

use chrono::Utc;
use orbit_automation::review::{LandedCandidate, classify_landing};
use orbit_common::OrbitError;
use orbit_engine::ReviewLandingRequest;
use orbit_engine::review_gate::revision;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    LandingTransformation, REVIEW_GATE_ARTIFACT, ReviewCertificate, ReviewInvalidation,
    ReviewLanding,
};
use serde_json::json;

use super::REVIEW_AUDIT;
use crate::OrbitRuntime;

/// Record the landing of a reviewed candidate. Missing evidence records an
/// uncovered landing rather than failing the completion that already
/// happened.
pub(crate) fn record_review_landing(
    runtime: &OrbitRuntime,
    request: &ReviewLandingRequest,
) -> Result<(), OrbitError> {
    let Some(certificate) = certificate_for_head(runtime, request)? else {
        tracing::warn!(
            run_id = %request.run_id,
            reviewed_head = %request.reviewed_head_sha,
            "managed completion found no certificate for the reviewed head; landing stays uncovered"
        );
        return Ok(());
    };
    let store = runtime.review_store()?;
    let branch = request.base.trim_start_matches("origin/").to_string();
    let landing = match request.landed_commit.as_deref() {
        // A merge this completion did not perform conditionally landed
        // content the gate never authorized, whatever tree it produced.
        Some(landed_commit) if !request.managed_merge => uncovered(
            &certificate,
            request,
            &branch,
            SourceRevision {
                commit: landed_commit.to_string(),
                tree: String::new(),
            },
            ReviewInvalidation::ExternalLandingRace,
        ),
        Some(landed_commit) => classify(request, &certificate, landed_commit, &branch),
        None => uncovered(
            &certificate,
            request,
            &branch,
            SourceRevision {
                commit: String::new(),
                tree: String::new(),
            },
            ReviewInvalidation::MappingUnknown,
        ),
    };
    store.review_landing_record(&landing)?;
    runtime.record_pipeline_audit(
        REVIEW_AUDIT,
        Some(&request.run_id),
        Some("system"),
        if landing.covered {
            AuditEventStatus::Success
        } else {
            AuditEventStatus::Failure
        },
        json!({
            "phase": "landing",
            "attempt_id": certificate.attempt_id,
            "pr_number": request.pr_number,
            "landed_commit": landing.landed.commit,
            "managed_merge": request.managed_merge,
            "transformation": landing.transformation,
            "covered": landing.covered,
            "reason": landing.reason,
            "recorded_at": Utc::now().to_rfc3339(),
        }),
        None,
    )?;
    Ok(())
}

/// The passed certificate whose final candidate is the reviewed head.
fn certificate_for_head(
    runtime: &OrbitRuntime,
    request: &ReviewLandingRequest,
) -> Result<Option<ReviewCertificate>, OrbitError> {
    for task_id in &request.task_ids {
        let Some(artifact) = runtime.get_task_artifact(task_id, REVIEW_GATE_ARTIFACT)? else {
            continue;
        };
        let Ok(certificate) = serde_json::from_slice::<ReviewCertificate>(&artifact.content) else {
            continue;
        };
        if certificate.final_candidate.commit == request.reviewed_head_sha
            && certificate.verdict.passed()
        {
            return Ok(Some(certificate));
        }
    }
    Ok(None)
}

fn classify(
    request: &ReviewLandingRequest,
    certificate: &ReviewCertificate,
    landed_commit: &str,
    branch: &str,
) -> ReviewLanding {
    let workspace = &request.workspace_path;
    // The merge commit lives on the remote; fetch it into the worktree's
    // shared object store before reading it. A fetch failure leaves the
    // landing uncovered with the reason recorded.
    let fetched = orbit_engine::review_gate::fetch_landed_commit(workspace, landed_commit);
    let landed = match fetched.and_then(|_| revision(workspace, landed_commit)) {
        Ok(landed) => landed,
        Err(error) => {
            tracing::warn!(
                run_id = %request.run_id,
                landed_commit,
                error = %error,
                "landed commit could not be read; landing stays uncovered"
            );
            return uncovered(
                certificate,
                request,
                branch,
                SourceRevision {
                    commit: landed_commit.to_string(),
                    tree: String::new(),
                },
                ReviewInvalidation::ObjectsMissing,
            );
        }
    };
    let facts = match orbit_engine::review_gate::landed_candidate_facts(
        workspace,
        &landed,
        &certificate.final_candidate.commit,
        &certificate.base.tree,
        certificate.implementation_commits.len() + certificate.repair_commits.len(),
    ) {
        Ok(facts) => facts,
        Err(error) => {
            tracing::warn!(
                run_id = %request.run_id,
                landed_commit,
                error = %error,
                "landing mapping could not be verified; landing stays uncovered"
            );
            return uncovered(
                certificate,
                request,
                branch,
                landed,
                ReviewInvalidation::MappingUnknown,
            );
        }
    };
    let landed_candidate = LandedCandidate {
        landed_commit: landed.commit.clone(),
        landed_tree: landed.tree.clone(),
        base_at_landing_tree: facts.base_at_landing.tree.clone(),
        is_candidate_commit: facts.is_candidate_commit,
        parents: facts.parents,
        span_commits: facts.span_commits,
    };
    let (transformation, covered, reason) = classify_landing(certificate, &landed_candidate);
    ReviewLanding {
        attempt_id: certificate.attempt_id.clone(),
        repository: certificate.repository.clone(),
        branch: branch.to_string(),
        pr_number: Some(request.pr_number.clone()),
        landed,
        base_at_landing: facts.base_at_landing,
        transformation,
        covered,
        reason: reason.map(|reason| reason.as_str().to_string()),
        recorded_at: Utc::now(),
    }
}

fn uncovered(
    certificate: &ReviewCertificate,
    request: &ReviewLandingRequest,
    branch: &str,
    landed: SourceRevision,
    reason: ReviewInvalidation,
) -> ReviewLanding {
    ReviewLanding {
        attempt_id: certificate.attempt_id.clone(),
        repository: certificate.repository.clone(),
        branch: branch.to_string(),
        pr_number: Some(request.pr_number.clone()),
        landed,
        base_at_landing: SourceRevision {
            commit: String::new(),
            tree: String::new(),
        },
        transformation: LandingTransformation::Unknown,
        covered: false,
        reason: Some(reason.as_str().to_string()),
        recorded_at: Utc::now(),
    }
}
