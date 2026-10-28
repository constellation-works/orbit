use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::workflow::handoff::{HandoffArtifactRef, HandoffReviewEvidence};
use orbit_types::workflow::{
    REVIEW_GATE_ARTIFACT, REVIEW_LANDING_DECISION_PENDING, REVIEW_MANIFEST_ARTIFACT,
    REVIEW_REPORT_ARTIFACT, ReviewCertificate, ReviewGateAdmitted, ReviewGateHistoryNote,
    ReviewManifest, ReviewTiming,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::review::gate::context::GateContext;
use crate::application::review::gate::judgement::review_fixes_section;

use super::model::Settled;

/// The settlement of a gate that did not apply, for `reason`.
pub(super) fn not_required(reason: Option<String>) -> Value {
    json!({
        "gate": "not_required",
        "reason": reason,
        "reviewed_head_sha": "",
        "reviewed_base_sha": "",
        "reviewer_fixed": false,
        "review_fixes": "",
        // Always present: a claimed leaf's handoff forwards it typed.
        "handoff_evidence": null,
    })
}

/// What a settled-without-approval review leaves the candidate waiting for.
pub(super) fn candidate_standing(admitted: &ReviewGateAdmitted) -> &'static str {
    if admitted.timing == ReviewTiming::BeforeLanding {
        REVIEW_LANDING_DECISION_PENDING
    } else {
        "the candidate stays unpublished until a recorded decision resumes delivery"
    }
}

pub(super) fn settled_outcome(
    runtime: &OrbitRuntime,
    context: &GateContext,
    certificate: ReviewCertificate,
) -> Result<Settled, OrbitError> {
    // An owner before-landing run already promoted its task to review and
    // leaves the PR open for a recorded decision. A claimed leaf has not
    // handed off yet: its owner's task is still in progress, so unavailable
    // evidence must release the claim with the same hold as before-PR review.
    let holds_evidence = context.claimed
        || context
            .admission
            .as_ref()
            .is_none_or(|admission| !admission.gates_landing());
    if !certificate.verdict.passed() {
        for task_id in context.task_ids.iter().filter(|_| holds_evidence) {
            if let Some(hold) = crate::application::review::evidence::evidence_hold(
                runtime, task_id,
            )?
            .filter(|hold| {
                hold.schema_version == 1
                    && crate::application::review::evidence::evidence_only(
                        &certificate,
                        &hold.requirements,
                    )
                    && hold.attempt_id == certificate.attempt_id
                    && hold.candidate == certificate.final_candidate
                    && hold.task_meaning_digest == certificate.task_meaning_digest
            }) {
                runtime.apply_task_automation_update(
                    task_id,
                    TaskAutomationUpdate {
                        expected_status: Some(orbit_types::task::TaskStatus::InProgress),
                        status: Some(orbit_types::task::TaskStatus::InProgress),
                        status_event: Some("review_awaiting_evidence".into()),
                        status_note: Some(ReviewGateHistoryNote::awaiting_evidence_text(
                            &context.run_id,
                            &hold.candidate.commit,
                            None,
                        )),
                        ..Default::default()
                    },
                )?;
                return Ok(Settled::AwaitingEvidence(Box::new(hold)));
            }
        }
        if !certificate.baseline_red.is_empty() {
            return Ok(Settled::BaselineRed(Box::new(certificate)));
        }
        return Ok(Settled::Blocked(Box::new(certificate)));
    }
    apply_mutation_proposals(runtime, context, &certificate)?;
    let mut output = passed_output(&certificate);
    if context.claimed {
        output["handoff_evidence"] =
            serde_json::to_value(handoff_evidence(runtime, context, &certificate)?).map_err(
                |error| OrbitError::Execution(format!("serialize review evidence: {error}")),
            )?;
    }
    Ok(Settled::Passed(output))
}

/// [granted-tools §4] Apply the mutation proposals this passed attempt's
/// manifest put in front of its reviewer, once each, before anything is
/// published or landed. A replayed settlement retries only what no applied
/// receipt covers. A claimed leaf applies nothing: its review read none, and
/// a live change is never made from a follower.
fn apply_mutation_proposals(
    runtime: &OrbitRuntime,
    context: &GateContext,
    certificate: &ReviewCertificate,
) -> Result<(), OrbitError> {
    if context.claimed {
        return Ok(());
    }
    let Some(task_id) = context.task_ids.first() else {
        return Ok(());
    };
    let reviewed = runtime
        .get_task_artifact(task_id, REVIEW_MANIFEST_ARTIFACT)?
        .and_then(|artifact| serde_json::from_slice::<ReviewManifest>(&artifact.content).ok())
        .filter(|manifest| manifest.attempt_id == certificate.attempt_id)
        .map(|manifest| manifest.mutation_proposals)
        .unwrap_or_default();
    runtime.apply_reviewed_mutation_proposals(
        &context.run_id,
        &certificate.attempt_id,
        &context.task_ids,
        &reviewed,
    )?;
    Ok(())
}

/// The before-PR evidence a claimed leaf hands off [ORB-13908]: the passed
/// verdict and the digests of the certificate, manifest and report the
/// owner holds for its task, which it re-reads and checks at acceptance.
fn handoff_evidence(
    runtime: &OrbitRuntime,
    context: &GateContext,
    certificate: &ReviewCertificate,
) -> Result<HandoffReviewEvidence, OrbitError> {
    let task_id = context
        .task_ids
        .first()
        .ok_or_else(|| OrbitError::Execution("review evidence names no task".to_string()))?;
    let reference = |path: &str| -> Result<Option<HandoffArtifactRef>, OrbitError> {
        Ok(runtime
            .get_task_artifact(task_id, path)?
            .map(|artifact| HandoffArtifactRef {
                path: path.to_string(),
                sha256: sha256_hex(&artifact.content),
            }))
    };
    let certificate_ref = reference(REVIEW_GATE_ARTIFACT)?.ok_or_else(|| {
        OrbitError::Execution(format!(
            "review_gate_stale: the owner holds no {REVIEW_GATE_ARTIFACT} for task '{task_id}'"
        ))
    })?;
    // [ORB-14478] Pin every host run the verdict counted: its result, then
    // its log.
    let host_evidence = certificate
        .host_evidence
        .iter()
        .filter(|record| record.passed)
        .flat_map(|record| [record.artifact.as_deref(), record.log_artifact.as_deref()])
        .map(|path| {
            let path = path.ok_or_else(|| {
                OrbitError::Execution(
                    "review_gate_stale: a passed host run names no result or log".to_string(),
                )
            })?;
            reference(path)?.ok_or_else(|| {
                OrbitError::Execution(format!(
                    "review_gate_stale: the owner holds no {path} for task '{task_id}'"
                ))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(HandoffReviewEvidence {
        attempt_id: certificate.attempt_id.clone(),
        verdict: certificate.verdict,
        reviewed_head_sha: certificate.final_candidate.commit.clone(),
        reviewed_base_sha: certificate.base.commit.clone(),
        reviewer_commit: certificate
            .repair_commits
            .last()
            .map(|repair| repair.commit.clone()),
        reviewer_crew: certificate.reviewer.crew.clone(),
        reviewer_run_id: context.run_id.clone(),
        certificate: certificate_ref,
        artifacts: [REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT]
            .into_iter()
            .map(reference)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect(),
        host_evidence,
    })
}

/// What the PR steps read from an accept. `reviewer_fixed` gates the owner
/// revalidation of the reviewer's commit, whose paths are checked from
/// `implementation_head_sha`; `review_fixes` is the PR body section.
fn passed_output(certificate: &ReviewCertificate) -> Value {
    json!({
        "gate": "passed",
        "verdict": certificate.verdict.as_str(),
        "assurance": certificate.assurance.map(|assurance| assurance.as_str()),
        "attempt_id": certificate.attempt_id,
        "reviewed_head_sha": certificate.final_candidate.commit,
        "reviewed_base_sha": certificate.base.commit,
        "implementation_head_sha": certificate.reviewed_candidate.commit,
        "final_candidate_tree": certificate.final_candidate.tree,
        "implementation_commits": certificate.implementation_commits.iter().map(|c| &c.commit).collect::<Vec<_>>(),
        "repair_commits": certificate.repair_commits.iter().map(|c| &c.commit).collect::<Vec<_>>(),
        "reviewer_fixed": !certificate.repair_commits.is_empty(),
        "review_fixes": review_fixes_section(certificate).unwrap_or_default(),
        "findings": certificate.findings.len(),
        "consumed": certificate.consumed,
        "certificate_artifact": REVIEW_GATE_ARTIFACT,
        "known_red": certificate.known_red,
        "handoff_evidence": null,
    })
}
