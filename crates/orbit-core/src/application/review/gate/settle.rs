//! Settle the reviewer's report into an honest verdict and certificate.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_engine::DispatchError;
use orbit_engine::review_gate::{candidate_identity_at, committed_paths, uncommitted_paths};
use orbit_store::contracts::{ClaimEvidence, ClaimWorkerUpdate, ReviewSettlement};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{HandoffArtifactRef, HandoffReviewEvidence};
use orbit_types::workflow::{
    CommitIdentity, REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, REVIEW_MANIFEST_ARTIFACT,
    REVIEW_REPORT_ARTIFACT, ReviewAttemptState, ReviewCertificate, ReviewerIdentity,
};
use serde_json::{Value, json};

use super::super::REVIEW_AUDIT;
use crate::OrbitRuntime;
use crate::application::task::TaskUpdateParams;

use super::admit::reviewer_identity;
use super::context::GateContext;
use super::judgement::{
    Judgement, repair_author_label, review_fixes_section, verdict_comment, write_artifact,
};

/// Close the admitted attempt with an honest verdict.
///
/// An accept returns the reviewed head and base the PR steps must recheck.
/// With the reviewer's fixes committed it also reports `reviewer_fixed`, so
/// the pipeline reruns owner validation and the ownership check on that head
/// before publishing, and the PR body's "Review fixes" section [ORB-13989].
/// Any other verdict refuses the step — a settled verdict is not retried and
/// never goes back to the implementer — so the pipeline's failure handoff
/// preserves the candidate and blocks the task with the escalation.
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
            "reviewer_fixed": false,
            "review_fixes": "",
            // Always present: a claimed leaf's handoff forwards it typed.
            "handoff_evidence": null,
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
        Ok(Settled::Blocked { certificate, .. }) => (
            AuditEventStatus::Success,
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
        Ok(Settled::Blocked {
            certificate,
            awaiting_evidence,
        }) => Err(DispatchError::DeterministicActionRefused {
            action: action.to_string(),
            message: format!(
                "{}: verdict {} ({}); {} finding(s) recorded; the candidate \
                 stays unpublished until a recorded decision resumes delivery",
                if awaiting_evidence {
                    "review_awaiting_evidence"
                } else {
                    "review_gate_blocked"
                },
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
    Blocked {
        certificate: Box<ReviewCertificate>,
        awaiting_evidence: bool,
    },
}

fn settle(
    runtime: &OrbitRuntime,
    context: &mut GateContext,
    attempt_id: &str,
    reviewer: ReviewerIdentity,
    admission_output: &Value,
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

    if attempt.index <= ledger.reset_through() {
        return Err(OrbitError::CapabilityDenied(
            "review_gate_stale: attempt retired by an operator reset; admit a fresh attempt".into(),
        ));
    }

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

    let validation_scope = validation_scope(context, &reviewed.commits, repair.as_ref())?;
    judgement.reconcile_verdict(repair.as_ref(), &validation_scope);
    let now = Utc::now();

    let settled = match recorded {
        Some(verdict) => {
            if verdict != judgement.verdict {
                return Err(OrbitError::Execution(format!(
                    "review_gate_stale: settlement_diverged: attempt {attempt_id} settled {} \
                     but its evidence now judges {}{}; a fresh reviewer start is required",
                    verdict.as_str(),
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
        required_validation_commands: judgement.required_validation_commands.clone(),
        validation_complete: judgement.validation_complete,
        retained_obligations: judgement.retained_obligations.clone(),
        validation_scope,
        reviewer,
        consumed: settled.consumed_for(&reviewed.head, &judgement.task_meaning_digest, now),
        budget: settled.budget,
        escalation: judgement.escalation.clone(),
        selectors_widened: judgement.selectors_widened.clone(),
        issued_at: now,
    };
    if super::super::evidence::evidence_only(&certificate, &judgement.external_evidence) {
        let hold = orbit_types::workflow::ReviewEvidenceHold {
            schema_version: 1,
            attempt_id: certificate.attempt_id.clone(),
            lineage_key: certificate.lineage_key.clone(),
            run_id: context.run_id.clone(),
            candidate: certificate.final_candidate.clone(),
            task_meaning_digest: certificate.task_meaning_digest.clone(),
            requirements: judgement.external_evidence,
        };
        let bytes = serde_json::to_vec_pretty(&hold)
            .map_err(|error| OrbitError::Execution(format!("serialize evidence hold: {error}")))?;
        for task in &context.tasks {
            write_artifact(
                runtime,
                &task.id,
                &context.run_id,
                orbit_types::workflow::REVIEW_EVIDENCE_HOLD_ARTIFACT,
                &bytes,
            )?;
        }
    }
    store.review_certificate_record(&context.workspace_id, &certificate)?;
    publish_certificate(runtime, context, &certificate)?;
    settled_outcome(runtime, context, certificate)
}

/// What validation sources are judged against: every bundle task's
/// selectors, as widened for reviewer repairs, plus a `file:` selector for
/// every path the implementation and repair commits changed.
fn validation_scope(
    context: &GateContext,
    implementation: &[CommitIdentity],
    repair: Option<&CommitIdentity>,
) -> Result<Vec<String>, OrbitError> {
    let mut scope = context
        .tasks
        .iter()
        .flat_map(|task| task.context_files.iter().cloned())
        .collect::<Vec<_>>();
    for commit in implementation.iter().chain(repair) {
        scope.extend(
            committed_paths(&context.workspace_path, &commit.commit)?
                .into_iter()
                .map(|path| format!("file:{path}")),
        );
    }
    scope.sort();
    scope.dedup();
    Ok(scope)
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
    settled_outcome(runtime, context, certificate)
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
            post_comment(runtime, context, &task.id, &comment)?;
        }
    }
    Ok(())
}

/// Post the verdict comment. A claimed leaf's comment crosses its binding to
/// the owner as claim evidence [ORB-13908].
fn post_comment(
    runtime: &OrbitRuntime,
    context: &GateContext,
    task_id: &str,
    comment: &str,
) -> Result<(), OrbitError> {
    if context.claimed {
        runtime.route_worker_tool(
            "orbit.task.update",
            json!({
                "id": task_id,
                "_worker_update": ClaimWorkerUpdate {
                    evidence: ClaimEvidence {
                        comment: Some(comment.to_string()),
                        ..ClaimEvidence::default()
                    },
                    ..ClaimWorkerUpdate::default()
                },
            }),
            Default::default(),
        )?;
        return Ok(());
    }
    runtime.update_task_as_system(
        task_id,
        TaskUpdateParams {
            comment: Some(comment.to_string()),
            ..TaskUpdateParams::default()
        },
        None,
    )?;
    Ok(())
}

fn settled_outcome(
    runtime: &OrbitRuntime,
    context: &GateContext,
    certificate: ReviewCertificate,
) -> Result<Settled, OrbitError> {
    if !certificate.verdict.passed() {
        let mut awaiting_evidence = false;
        for task_id in &context.task_ids {
            awaiting_evidence |= super::super::evidence::evidence_hold(runtime, task_id)?
                .is_some_and(|hold| {
                    hold.schema_version == 1
                        && super::super::evidence::evidence_only(&certificate, &hold.requirements)
                        && hold.attempt_id == certificate.attempt_id
                        && hold.candidate == certificate.final_candidate
                        && hold.task_meaning_digest == certificate.task_meaning_digest
                });
        }
        return Ok(Settled::Blocked {
            awaiting_evidence,
            certificate: Box::new(certificate),
        });
    }
    let mut output = passed_output(&certificate);
    if context.claimed {
        output["handoff_evidence"] =
            serde_json::to_value(handoff_evidence(runtime, context, &certificate)?).map_err(
                |error| OrbitError::Execution(format!("serialize review evidence: {error}")),
            )?;
    }
    Ok(Settled::Passed(output))
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
        "handoff_evidence": null,
    })
}
