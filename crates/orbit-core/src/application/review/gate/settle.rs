//! Settle the reviewer's report into an honest verdict and certificate.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_engine::review_gate::{candidate_identity_at, committed_paths, uncommitted_paths};
use orbit_store::contracts::ReviewSettlement;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    FindingDisposition, REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, ReviewAttemptState,
    ReviewCertificate, ReviewerIdentity,
};
use serde_json::{Value, json};

use super::super::REVIEW_AUDIT;
use crate::OrbitRuntime;
use crate::application::task::TaskUpdateParams;

use super::admit::reviewer_identity;
use super::context::GateContext;
use super::judgement::{Judgement, repair_author_label, verdict_comment, write_artifact};

/// Close the admitted attempt with an honest verdict.
///
/// A pass returns the reviewed head and base the PR steps must recheck. A
/// `changes_required` verdict the lineage can still afford to rework returns
/// `gate: rework_required` with the open findings, so the pipeline's review
/// loop hands them to the implementer and reviews the new head [ORB-13891].
/// Any other non-pass refuses the step — a settled verdict is not retried —
/// so the pipeline's failure handoff preserves the candidate and blocks the
/// task with the escalation.
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

    // Only the pre-PR review loop can rework; a re-review of a published
    // head has no implementer step to send findings to.
    let rework_allowed = input.get("rework_allowed").and_then(Value::as_bool) == Some(true);
    let outcome = settle(
        runtime,
        &mut context,
        &attempt_id,
        reviewer,
        &admission_output,
        rework_allowed,
    );
    let (status, decision, error) = match &outcome {
        Ok(Settled::Passed(value)) => (AuditEventStatus::Success, value.clone(), None),
        Ok(Settled::Rework(value)) => (
            AuditEventStatus::Success,
            json!({
                "verdict": value["verdict"],
                "gate": value["gate"],
                "open_findings": value["rework"]["findings"].as_array().map_or(0, Vec::len),
            }),
            None,
        ),
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
        Ok(Settled::Passed(value) | Settled::Rework(value)) => Ok(value),
        Ok(Settled::Blocked { certificate }) => Err(DispatchError::DeterministicActionRefused {
            action: action.to_string(),
            message: format!(
                "review_gate_blocked: verdict {} ({}); {} finding(s) recorded; the candidate \
                 stays unpublished until a recorded decision resumes delivery",
                certificate.verdict.as_str(),
                certificate
                    .escalation
                    .as_deref()
                    .unwrap_or("no escalation reason recorded"),
                certificate.findings.len()
            ),
        }),
        Err(error) => Err(failed(error.to_string())),
    }
}

enum Settled {
    Passed(Value),
    /// `changes_required`, sent back to the implementer within the run.
    Rework(Value),
    Blocked {
        certificate: Box<ReviewCertificate>,
    },
}

fn settle(
    runtime: &OrbitRuntime,
    context: &mut GateContext,
    attempt_id: &str,
    reviewer: ReviewerIdentity,
    admission_output: &Value,
    rework_allowed: bool,
) -> Result<Settled, OrbitError> {
    let store = runtime.review_store()?;
    // The admission names the lineage it reserved in; a resumed run reuses
    // that admission, so settlement never re-derives a different key.
    let lineage_key = admission_output
        .get("lineage_key")
        .and_then(Value::as_str)
        .filter(|key| !key.trim().is_empty())
        .map_or_else(|| context.lineage_key(), ToOwned::to_owned);
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

    // A replay after the certificate was recorded reconciles it instead of
    // judging the candidate twice. A ledger that settled without one is an
    // interrupted settlement, finished below from the same evidence. A
    // released attempt — its reviewer step failed or its run ended — had no
    // verdict, so a resumed run settles it like an open one, unless a later
    // admission already superseded it.
    let released = attempt.released_at.is_some();
    if released && ledger.attempts.last().map(|last| &last.attempt_id) != Some(&attempt.attempt_id)
    {
        return Err(OrbitError::Execution(format!(
            "review_gate_stale: attempt {attempt_id} was released and a later reviewer start \
             superseded it"
        )));
    }
    let recorded = match attempt.state {
        ReviewAttemptState::Settled { verdict } if !released => Some(verdict),
        ReviewAttemptState::Settled { .. } | ReviewAttemptState::Open => None,
    };
    if recorded.is_some()
        && let Some(certificate) = store.review_certificate(&context.workspace_id, attempt_id)?
    {
        return reconcile_settled(runtime, context, certificate);
    }

    let head = orbit_engine::review_gate::revision(&context.workspace_path, "HEAD")?;
    let committed_repair = if head.commit == attempt.candidate.commit {
        None
    } else {
        // Only this attempt's own repair commit may sit on the candidate.
        let repair = orbit_engine::review_gate::review_repair_at_head(
            &context.workspace_path,
            &attempt.candidate.commit,
            attempt_id,
            &repair_author_label(&reviewer),
        )?
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "review_gate_stale: candidate_changed: the worktree head is {} but attempt \
                 {attempt_id} admitted {}; only one worker may settle a candidate",
                head.commit, attempt.candidate.commit
            ))
        })?;
        Some(repair)
    };
    let resumed = committed_repair.is_some() || recorded.is_some();
    if resumed {
        let dirty = uncommitted_paths(&context.workspace_path)?;
        if !dirty.is_empty() {
            return Err(OrbitError::Execution(format!(
                "review_gate_stale: candidate_changed: attempt {attempt_id} already committed \
                 or settled its repairs but the worktree has further changes to {}",
                dirty.join(", ")
            )));
        }
    }
    let reviewed = candidate_identity_at(
        &context.workspace_path,
        &context.base_sha()?,
        &attempt.candidate.commit,
    )?;

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
    let repair = match committed_repair {
        Some(commit) => {
            let paths = committed_paths(&context.workspace_path, &commit.commit)?;
            judgement.adopt_repairs(runtime, context, &paths, &admitted_selectors)?;
            Some(commit)
        }
        // A settled ledger never charged a repair that was not committed.
        None if recorded.is_some() => None,
        None => judgement.commit_repairs(runtime, context, &reviewer, &attempt)?,
    };
    let reviewer_repair_cycles = u32::from(repair.is_some());

    // A resumed settlement judges against the ledger as it stood before its
    // own charge, with the elapsed time it already recorded.
    let judged_ledger = if recorded.is_some() || released {
        ledger.before_settling(attempt_id)
    } else {
        Some(ledger.clone())
    }
    .ok_or_else(|| {
        OrbitError::Execution(format!(
            "review_gate_stale: attempt {attempt_id} is not part of lineage '{lineage_key}'"
        ))
    })?;
    judgement.reconcile_verdict(&judged_ledger, repair.as_ref());
    let now = Utc::now();
    // A granted rework is the lineage's next repair cycle, charged with the
    // attempt that asked for it so the bound survives a resume.
    let rework_requested =
        rework_allowed && judgement.request_rework(&judged_ledger, reviewer_repair_cycles, now);
    let repair_cycles = reviewer_repair_cycles + u32::from(rework_requested);

    let settled = match recorded {
        Some(verdict) => {
            if verdict != judgement.verdict || attempt.repair_cycles != repair_cycles {
                return Err(OrbitError::Execution(format!(
                    "review_gate_stale: settlement_diverged: attempt {attempt_id} settled {} \
                     with {} repair cycle(s) but its evidence now judges {} with {repair_cycles}{}; \
                     a fresh reviewer start is required",
                    verdict.as_str(),
                    attempt.repair_cycles,
                    judgement.verdict.as_str(),
                    judgement
                        .escalation
                        .as_deref()
                        .map(|reason| format!(" ({reason})"))
                        .unwrap_or_default(),
                )));
            }
            ledger.as_of(attempt_id).unwrap_or(ledger)
        }
        // The store charges the attempt's recorded reviewer runtime; the
        // minutes budget gates new starts, so an admitted reviewer that
        // overruns it still settles on its evidence.
        None => store.review_settle(
            &context.workspace_id,
            &ReviewSettlement {
                lineage_key: &lineage_key,
                attempt_id,
                verdict: judgement.verdict,
                repair_cycles,
                now,
            },
        )?,
    };

    let final_candidate = match &repair {
        Some(commit) => SourceRevision {
            commit: commit.commit.clone(),
            tree: commit.tree.clone(),
        },
        None => reviewed.head.clone(),
    };
    let certificate = ReviewCertificate {
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: attempt_id.to_string(),
        lineage_key: lineage_key.clone(),
        task_ids: context.task_ids.clone(),
        task_meaning_digest: judgement.task_meaning_digest.clone(),
        repository: context.repository.clone(),
        base: reviewed.base.clone(),
        reviewed_candidate: reviewed.head.clone(),
        final_candidate,
        implementation_commits: reviewed.commits.clone(),
        repair_commits: repair.into_iter().collect(),
        verdict: judgement.verdict,
        assurance: judgement.verdict.assurance(),
        findings: judgement.findings.clone(),
        validation: judgement.validation.clone(),
        validation_complete: judgement.validation_complete,
        reviewer,
        consumed: settled.consumed(),
        budget: settled.budget,
        escalation: judgement.escalation.clone(),
        selectors_widened: judgement.selectors_widened.clone(),
        rework_requested,
        issued_at: now,
    };
    store.review_certificate_record(&context.workspace_id, &certificate)?;
    publish_certificate(runtime, context, &certificate)?;
    Ok(settled_outcome(certificate))
}

fn reconcile_settled(
    runtime: &OrbitRuntime,
    context: &GateContext,
    certificate: ReviewCertificate,
) -> Result<Settled, OrbitError> {
    let attempt_id = certificate.attempt_id.as_str();
    if certificate.verdict.passed() {
        if context.task_digests.1 != certificate.task_meaning_digest {
            return Err(OrbitError::Execution(format!(
                "review_gate_stale: task_meaning_changed: task meaning no longer matches \
                 settled certificate {attempt_id}"
            )));
        }
        let head = orbit_engine::review_gate::revision(&context.workspace_path, "HEAD")?;
        if head != certificate.final_candidate {
            return Err(OrbitError::Execution(format!(
                "review_gate_stale: candidate_changed: the worktree head is {} but certificate \
                 {attempt_id} settled {}",
                head.commit, certificate.final_candidate.commit
            )));
        }
    }
    // Settlement may have stopped before every task carried the evidence.
    publish_certificate(runtime, context, &certificate)?;
    Ok(settled_outcome(certificate))
}

/// Give every task the certificate artifact and verdict comment, writing
/// only what is missing so a replay after a partial publish neither loses
/// nor duplicates evidence. A task already carrying a later attempt's
/// certificate keeps it.
fn publish_certificate(
    runtime: &OrbitRuntime,
    context: &GateContext,
    certificate: &ReviewCertificate,
) -> Result<(), OrbitError> {
    let certificate_bytes = serde_json::to_vec_pretty(certificate)
        .map_err(|error| OrbitError::Execution(format!("serialize review certificate: {error}")))?;
    let comment = verdict_comment(certificate);
    for task in &context.tasks {
        let current = runtime.get_task_artifact(&task.id, REVIEW_GATE_ARTIFACT)?;
        let current_matches = current
            .as_ref()
            .is_some_and(|artifact| artifact.content == certificate_bytes);
        let superseded = current
            .as_ref()
            .and_then(|artifact| {
                serde_json::from_slice::<ReviewCertificate>(&artifact.content).ok()
            })
            .is_some_and(|held| {
                held.attempt_id != certificate.attempt_id && held.issued_at > certificate.issued_at
            });
        if superseded {
            continue;
        }
        if !current_matches {
            write_artifact(
                runtime,
                &task.id,
                &context.run_id,
                REVIEW_GATE_ARTIFACT,
                &certificate_bytes,
            )?;
        }
        let disclosed = runtime
            .get_task_comments(&task.id)?
            .iter()
            .any(|existing| existing.message.trim() == comment.trim());
        if !disclosed {
            runtime.update_task_as_system(
                &task.id,
                TaskUpdateParams {
                    comment: Some(comment.clone()),
                    ..TaskUpdateParams::default()
                },
                None,
            )?;
        }
    }
    Ok(())
}

fn settled_outcome(certificate: ReviewCertificate) -> Settled {
    if certificate.verdict.passed() {
        Settled::Passed(passed_output(&certificate))
    } else if certificate.rework_requested {
        Settled::Rework(rework_output(&certificate))
    } else {
        Settled::Blocked {
            certificate: Box::new(certificate),
        }
    }
}

/// What the implementer's rework step receives: the open findings to
/// address and the head they were found on, which the rework commit pins.
/// `reviewed_head_sha` stays empty and `gate` names the rework, so nothing
/// downstream can mistake this for a reviewed candidate.
fn rework_output(certificate: &ReviewCertificate) -> Value {
    let open = certificate
        .findings
        .iter()
        .filter(|finding| finding.disposition == FindingDisposition::Open)
        .collect::<Vec<_>>();
    json!({
        "gate": "rework_required",
        "verdict": certificate.verdict.as_str(),
        "attempt_id": certificate.attempt_id,
        "reviewed_head_sha": "",
        "reviewed_base_sha": "",
        "rework": {
            "attempt_id": certificate.attempt_id,
            "head_sha": certificate.final_candidate.commit,
            "base_sha": certificate.base.commit,
            "findings": open,
            "escalation": certificate.escalation,
            "repair_commits": certificate.repair_commits.iter().map(|c| &c.commit).collect::<Vec<_>>(),
            "remaining_repair_cycles": certificate.budget.repair_cycles.saturating_sub(certificate.consumed.repair_cycles),
            "remaining_reviewer_starts": certificate.budget.reviewer_starts.saturating_sub(certificate.consumed.reviewer_starts),
            "certificate_artifact": REVIEW_GATE_ARTIFACT,
        },
    })
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
