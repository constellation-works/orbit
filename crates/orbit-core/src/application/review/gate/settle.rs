//! Settle the reviewer's report into an honest verdict and certificate.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_engine::review_gate::candidate_identity;
use orbit_store::contracts::{ReviewSettlement, ReviewStoreBackend};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, ReviewAttemptState, ReviewCertificate,
    ReviewerIdentity,
};
use serde_json::{Value, json};

use super::super::REVIEW_AUDIT;
use crate::OrbitRuntime;
use crate::application::task::TaskUpdateParams;

use super::admit::reviewer_identity;
use super::context::GateContext;
use super::judgement::{Judgement, verdict_comment, write_artifact};

/// Close the admitted attempt with an honest verdict.
///
/// A pass returns the reviewed head and base the PR steps must recheck; a
/// non-pass fails the step so the pipeline's failure handoff preserves the
/// candidate and blocks the task with the escalation.
pub(crate) fn review_gate_settle(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let failed = |message: String| DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message,
    };
    let admission_output = input.get("admission").cloned().unwrap_or(Value::Null);
    if admission_output.get("applies").and_then(Value::as_bool) != Some(true) {
        return Ok(json!({
            "gate": "not_required",
            "reason": admission_output.get("reason").cloned().unwrap_or(Value::Null),
            "reviewed_head_sha": "",
            "reviewed_base_sha": "",
        }));
    }
    let mut settle_input = input.clone();
    if let (Some(object), Some(base_sha)) = (
        settle_input.as_object_mut(),
        admission_output.get("base_sha").cloned(),
    ) {
        object.insert("base_sha".to_string(), base_sha);
    }
    let mut context = GateContext::load(runtime, &settle_input, None)
        .map_err(|error| failed(error.to_string()))?;
    if context.admission.is_none() {
        return Err(failed(
            "review_gate_stale: the run no longer carries a review admission".to_string(),
        ));
    }
    let attempt_id = admission_output
        .get("attempt_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| failed("review_gate_settle requires admission.attempt_id".to_string()))?
        .to_string();
    let reviewer = reviewer_identity(runtime, &context, &admission_output)
        .map_err(|error| failed(error.to_string()))?;

    let outcome = settle(
        runtime,
        &mut context,
        &attempt_id,
        reviewer,
        &admission_output,
    );
    let (status, decision, error) = match &outcome {
        Ok(Settled::Passed(value)) => (AuditEventStatus::Success, value.clone(), None),
        Ok(Settled::Blocked { certificate }) => (
            AuditEventStatus::Failure,
            json!({
                "verdict": certificate.verdict.as_str(),
                "escalation": certificate.escalation,
            }),
            None,
        ),
        Err(error) => (
            AuditEventStatus::Failure,
            json!("refused"),
            Some(error.to_string()),
        ),
    };
    runtime
        .record_pipeline_audit(
            REVIEW_AUDIT,
            Some(&context.run_id),
            Some("system"),
            status,
            json!({
                "phase": "settle",
                "run_id": context.run_id,
                "task_ids": context.task_ids,
                "attempt_id": attempt_id,
                "outcome": decision,
                "recorded_at": Utc::now().to_rfc3339(),
            }),
            error,
        )
        .map_err(|error| failed(error.to_string()))?;

    match outcome {
        Ok(Settled::Passed(value)) => Ok(value),
        Ok(Settled::Blocked { certificate }) => Err(failed(format!(
            "review_gate_blocked: verdict {} ({}); {} finding(s) recorded; the candidate stays \
             unpublished until a recorded decision resumes delivery",
            certificate.verdict.as_str(),
            certificate
                .escalation
                .as_deref()
                .unwrap_or("no escalation reason recorded"),
            certificate.findings.len()
        ))),
        Err(error) => Err(failed(error.to_string())),
    }
}

enum Settled {
    Passed(Value),
    Blocked { certificate: Box<ReviewCertificate> },
}

fn settle(
    runtime: &OrbitRuntime,
    context: &mut GateContext,
    attempt_id: &str,
    reviewer: ReviewerIdentity,
    admission_output: &Value,
) -> Result<Settled, OrbitError> {
    let store = runtime.review_store()?;
    let lineage_key = context.lineage_key();
    let ledger = store
        .review_ledger(&context.workspace_id, &lineage_key)?
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "review_gate_stale: lineage '{lineage_key}' has no ledger for attempt {attempt_id}"
            ))
        })?;
    let attempt = ledger
        .attempts
        .iter()
        .find(|attempt| attempt.attempt_id == attempt_id)
        .cloned()
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "review_gate_stale: attempt {attempt_id} is not part of lineage '{lineage_key}'"
            ))
        })?;

    // A replay after the attempt already settled reconciles the recorded
    // certificate instead of judging the candidate twice.
    if let ReviewAttemptState::Settled { .. } = attempt.state {
        return reconcile_settled(context, store.as_ref(), attempt_id);
    }

    let reviewed = candidate_identity(&context.workspace_path, &context.base_sha()?)?;
    if reviewed.head.commit != attempt.candidate.commit {
        return Err(OrbitError::Execution(format!(
            "review_gate_stale: candidate_changed: the worktree head is {} but attempt \
             {attempt_id} admitted {}; only one worker may settle a candidate",
            reviewed.head.commit, attempt.candidate.commit
        )));
    }

    let mut judgement = Judgement::from_report(runtime, context, &attempt)?;
    let admitted_selectors = admission_output
        .get("task_selectors")
        .cloned()
        .map(serde_json::from_value::<BTreeMap<String, Vec<String>>>)
        .transpose()
        .map_err(|error| {
            OrbitError::InvalidInput(format!("admission.task_selectors is unreadable: {error}"))
        })?
        .unwrap_or_default();
    judgement.check_task_meaning(context, &attempt, &admitted_selectors)?;
    let repair = judgement.commit_repairs(runtime, context, &reviewer, &attempt)?;
    judgement.reconcile_verdict(&ledger, repair.as_ref());
    let now = Utc::now();
    let elapsed_seconds = attempt.elapsed_at(now);
    judgement.enforce_wall_time(&ledger, elapsed_seconds);

    let final_candidate = match &repair {
        Some(commit) => SourceRevision {
            commit: commit.commit.clone(),
            tree: commit.tree.clone(),
        },
        None => reviewed.head.clone(),
    };
    let repair_cycles = u32::from(repair.is_some());
    let ledger = store.review_settle(
        &context.workspace_id,
        &ReviewSettlement {
            lineage_key: &lineage_key,
            attempt_id,
            verdict: judgement.verdict,
            repair_cycles,
            elapsed_seconds,
            now,
        },
    )?;

    let certificate = ReviewCertificate {
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: attempt_id.to_string(),
        lineage_key: lineage_key.clone(),
        task_ids: context.task_ids.clone(),
        task_meaning_digest: judgement.task_meaning_digest.clone(),
        repository: context.repository.clone(),
        base: reviewed.base.clone(),
        reviewed_candidate: reviewed.head.clone(),
        final_candidate: final_candidate.clone(),
        implementation_commits: reviewed.commits.clone(),
        repair_commits: repair.clone().into_iter().collect(),
        verdict: judgement.verdict,
        assurance: judgement.verdict.assurance(),
        findings: judgement.findings.clone(),
        validation: judgement.validation.clone(),
        validation_complete: judgement.validation_complete,
        reviewer,
        consumed: ledger.consumed(),
        budget: ledger.budget,
        escalation: judgement.escalation.clone(),
        selectors_widened: judgement.selectors_widened.clone(),
        issued_at: now,
    };
    store.review_certificate_record(&context.workspace_id, &certificate)?;
    let certificate_bytes = serde_json::to_vec_pretty(&certificate)
        .map_err(|error| OrbitError::Execution(format!("serialize review certificate: {error}")))?;
    for task in &context.tasks {
        write_artifact(
            runtime,
            &task.id,
            &context.run_id,
            REVIEW_GATE_ARTIFACT,
            &certificate_bytes,
        )?;
        runtime.update_task(
            &task.id,
            TaskUpdateParams {
                comment: Some(verdict_comment(&certificate, &reviewed)),
                ..TaskUpdateParams::default()
            },
        )?;
    }

    if certificate.verdict.passed() {
        Ok(Settled::Passed(passed_output(&certificate)))
    } else {
        Ok(Settled::Blocked {
            certificate: Box::new(certificate),
        })
    }
}

fn reconcile_settled(
    context: &GateContext,
    store: &dyn ReviewStoreBackend,
    attempt_id: &str,
) -> Result<Settled, OrbitError> {
    let certificate = store
        .review_certificate(&context.workspace_id, attempt_id)?
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "review_gate_stale: attempt {attempt_id} settled without a recorded certificate; \
                 a fresh reviewer start is required"
            ))
        })?;
    if !certificate.verdict.passed() {
        return Ok(Settled::Blocked {
            certificate: Box::new(certificate),
        });
    }
    let head = orbit_engine::review_gate::revision(&context.workspace_path, "HEAD")?;
    if head != certificate.final_candidate {
        return Err(OrbitError::Execution(format!(
            "review_gate_stale: candidate_changed: the worktree head is {} but certificate \
             {attempt_id} settled {}",
            head.commit, certificate.final_candidate.commit
        )));
    }
    Ok(Settled::Passed(passed_output(&certificate)))
}

fn passed_output(certificate: &ReviewCertificate) -> Value {
    json!({
        "gate": "passed",
        "verdict": certificate.verdict.as_str(),
        "assurance": certificate.assurance.map(|assurance| assurance.as_str()),
        "attempt_id": certificate.attempt_id,
        "reviewed_head_sha": certificate.final_candidate.commit,
        "reviewed_base_sha": certificate.base.commit,
        "final_candidate_tree": certificate.final_candidate.tree,
        "implementation_commits": certificate.implementation_commits.iter().map(|c| &c.commit).collect::<Vec<_>>(),
        "repair_commits": certificate.repair_commits.iter().map(|c| &c.commit).collect::<Vec<_>>(),
        "findings": certificate.findings.len(),
        "consumed": certificate.consumed,
        "certificate_artifact": REVIEW_GATE_ARTIFACT,
    })
}
