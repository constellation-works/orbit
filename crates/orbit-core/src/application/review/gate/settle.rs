//! Settle the reviewer's report into an honest verdict and certificate.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_engine::review_gate::{candidate_identity_at, committed_paths, uncommitted_paths};
use orbit_engine::{DispatchError, RuntimeHost, TaskAutomationUpdate};
use orbit_store::contracts::{ClaimEvidence, ClaimWorkerUpdate, ReviewSettlement};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::handoff::{HandoffArtifactRef, HandoffReviewEvidence};
use orbit_types::workflow::{
    CommitIdentity, REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, REVIEW_MANIFEST_ARTIFACT,
    REVIEW_REPORT_ARTIFACT, ReviewAttemptState, ReviewCertificate, ReviewTiming, ReviewerIdentity,
};
use serde_json::{Value, json};

use super::super::REVIEW_AUDIT;
use crate::OrbitRuntime;
use crate::application::task::TaskUpdateParams;

use super::admit::reviewer_identity;
use super::baseline;
use super::context::GateContext;
use super::judgement::{
    Judgement, repair_author_label, review_fixes_section, verdict_comment, write_artifact,
};
use super::owed::{EVIDENCE_RECEIVED_DECISION, owed_evidence, owed_hold_received};

/// Close the admitted attempt with an honest verdict.
///
/// An accept returns the reviewed head and base the PR steps must recheck.
/// With the reviewer's fixes committed it also reports `reviewer_fixed`, so
/// the pipeline reruns owner validation and the ownership check on that head
/// before publishing, and the PR body's "Review fixes" section [ORB-13989].
/// An evidence-only verdict holds delivery without recovery. Other verdicts
/// refuse the step — a settled verdict is not retried and
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
        Ok(Settled::AwaitingEvidence(hold)) => (
            AuditEventStatus::Success,
            json!({"gate": "awaiting_evidence", "evidence_hold": hold}),
            None,
        ),
        Ok(Settled::Blocked(certificate)) => (
            AuditEventStatus::Success,
            json!({
                "verdict": certificate.verdict.as_str(),
                "escalation": certificate.escalation,
            }),
            None,
        ),
        Ok(Settled::BaselineRed(certificate)) => (
            AuditEventStatus::Success,
            baseline::audit_outcome(&certificate.baseline_red),
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
        Ok(Settled::AwaitingEvidence(hold)) => Err(DispatchError::ReviewEvidenceHold(hold)),
        Ok(Settled::Blocked(certificate)) => Err(DispatchError::DeterministicActionRefused {
            action: action.to_string(),
            message: format!(
                "review_gate_blocked: verdict {} ({}); {} finding(s) recorded; {}",
                certificate.verdict.as_str(),
                certificate
                    .escalation
                    .as_deref()
                    .unwrap_or("no escalation reason recorded"),
                certificate.findings.len(),
                if admission_output.get("timing").and_then(Value::as_str)
                    == Some(ReviewTiming::BeforeLanding.as_str())
                {
                    "the pull request stays open and unmerged until a recorded decision lands it"
                } else {
                    "the candidate stays unpublished until a recorded decision resumes delivery"
                }
            ),
        }),
        // Typed `[baseline_red]`, so the failure handoff keeps the candidate
        // and holds the task until the base passes [ORB-14434].
        Ok(Settled::BaselineRed(certificate)) => Err(DispatchError::DeterministicActionRefused {
            action: action.to_string(),
            message: baseline::baseline_red_refusal(&certificate.baseline_red, &attempt_id)
                .unwrap_or_default(),
        }),
        Err(error) => Err(failed(error.to_string())),
    }
}

enum Settled {
    Passed(Value),
    AwaitingEvidence(Box<orbit_types::workflow::ReviewEvidenceHold>),
    Blocked(Box<ReviewCertificate>),
    /// Every failed required check fails the same way on the pinned base,
    /// and nothing else keeps the review from passing.
    BaselineRed(Box<ReviewCertificate>),
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

    // A held review whose owed evidence arrived settles without a reviewer,
    // on exactly the clean candidate its admission pinned.
    let held = if admission_output.get("decision").and_then(Value::as_str)
        == Some(EVIDENCE_RECEIVED_DECISION)
    {
        if head.commit != attempt.candidate.commit
            || !uncommitted_paths(&context.workspace_path)?.is_empty()
        {
            return Err(OrbitError::Execution(format!(
                "review_gate_stale: candidate_changed: attempt {attempt_id} settles a held \
                 review without a reviewer, but the worktree no longer holds the admitted \
                 candidate {}",
                attempt.candidate.commit
            )));
        }
        let held_attempt = admission_output
            .get("held_attempt_id")
            .and_then(Value::as_str);
        Some(
            owed_hold_received(runtime, context, &reviewed)?
                .filter(|(hold, _)| Some(hold.attempt_id.as_str()) == held_attempt)
                .ok_or_else(|| {
                    OrbitError::Execution(format!(
                        "review_gate_stale: the held review admission {attempt_id} resumed no \
                         longer settles without a reviewer; admit a fresh attempt"
                    ))
                })?,
        )
    } else {
        None
    };
    let mut judgement = match &held {
        Some((hold, certificate)) => Judgement::from_held_certificate(context, hold, certificate),
        None => Judgement::from_report(runtime, context, &attempt)?,
    };
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

    let validation_scope = validation_scope(context, &reviewed.commits, repair.as_ref(), &[])?;
    let final_candidate = match &repair {
        Some(commit) => SourceRevision {
            commit: commit.commit.clone(),
            tree: commit.tree.clone(),
        },
        None => reviewed.head.clone(),
    };
    let carry = match context.task_ids.as_slice() {
        [task_id] => super::super::evidence::evidence_carry(
            runtime,
            task_id,
            &context.workspace_path,
            &reviewed.base,
            &final_candidate,
        )?,
        _ => super::super::evidence::EvidenceCarry::None,
    };
    // Checks a host-evidence rule owes for what this candidate changed are
    // required whatever the reviewer reported.
    let owed = match &held {
        Some((_, certificate)) => certificate.owed_evidence.clone(),
        None => owed_evidence(runtime, context, reviewed.commits.iter().chain(&repair))?,
    };
    judgement.require_owed_evidence(&owed);
    // [ORB-14478] A claimed leaf's host runs the sandbox-gated checks its
    // reviewer named, before the verdict counts the evidence.
    let host = judgement.fulfil_host_evidence(
        runtime,
        context,
        attempt_id,
        &final_candidate,
        &validation_scope,
    )?;
    judgement.reconcile_external_evidence(
        runtime,
        context,
        &final_candidate,
        repair.as_ref(),
        &validation_scope,
        carry.carried(),
        host,
    )?;
    // [ORB-14434] Check the reviewer's red-base claims on the final
    // candidate before the verdict is reconciled: a refused claim settles
    // the review incomplete.
    let baseline_red = judgement.verify_baseline_claims(
        runtime,
        context,
        &reviewed.base,
        &context.base_ref(),
        &validation_scope,
    )?;
    // [ORB-14616] Every file a control mutated must come back byte-identical
    // in the final candidate.
    let unrestored = judgement.unrestored_mutation(
        &context.workspace_path,
        &reviewed.head.commit,
        &final_candidate.commit,
    )?;
    judgement.reconcile_verdict(repair.as_ref(), &validation_scope, unrestored.as_ref());
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
        baseline_commands: judgement.baseline_commands.clone(),
        validation_complete: judgement.validation_complete,
        retained_obligations: judgement.retained_obligations.clone(),
        retired_validation: judgement.retired_validation.clone(),
        validation_scope,
        reviewer,
        // The attempt was reserved under its admission digest. Selector
        // widening replaces `judgement.task_meaning_digest` with the
        // post-widening value, which `consumed_for` does not match, so the
        // certificate would record zero reviewer runtime. Coverage and
        // replay still bind to that post-widening digest above.
        consumed: settled.consumed_for(&reviewed.head, &attempt.task_meaning_digest, now),
        budget: settled.budget,
        escalation: judgement.escalation.clone(),
        selectors_widened: judgement.selectors_widened.clone(),
        evidence_carried: judgement.evidence_carried.clone(),
        baseline_red,
        host_evidence: judgement.host_evidence.clone(),
        owed_evidence: owed,
        resumed_hold_attempt: held.map(|(hold, _)| hold.attempt_id),
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
            task_spec_digest: context
                .tasks
                .first()
                .map(orbit_types::task::Task::spec_digest),
            published_ref: None,
        };
        let hold = orbit_types::workflow::ReviewEvidenceHold {
            published_ref: if context.claimed {
                publish_held_candidate(context, &hold.candidate.commit)
            } else {
                None
            },
            ..hold
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

/// Publish a claimed leaf's held candidate on `origin`, where the owner
/// fetches it to run a named check. Best-effort: a failed push leaves the
/// hold intact, and the owner then reports the candidate unreachable instead
/// of fulfilling it. Returns the ref it was published to.
fn publish_held_candidate(context: &GateContext, commit: &str) -> Option<String> {
    match orbit_engine::review_gate::publish_held_candidate(&context.workspace_path, commit) {
        Ok(target) => {
            tracing::info!(
                target: "orbit.core.review",
                run_id = %context.run_id,
                target = %target,
                commit,
                "published the held candidate for owner evidence"
            );
            Some(target)
        }
        Err(error) => {
            tracing::warn!(
                target: "orbit.core.review",
                run_id = %context.run_id,
                commit,
                "could not publish the held candidate for owner evidence: {error}"
            );
            None
        }
    }
}

/// What validation sources are judged against: every bundle task's
/// selectors, as widened for reviewer repairs, plus a `file:` selector for
/// every path the implementation and repair commits changed. `pending` adds
/// reviewer edits settlement has not committed yet, which it would commit
/// as the repair.
pub(super) fn validation_scope(
    context: &GateContext,
    implementation: &[CommitIdentity],
    repair: Option<&CommitIdentity>,
    pending: &[String],
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
    scope.extend(pending.iter().map(|path| format!("file:{path}")));
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
        runtime.route_worker_host_tool(
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
    // [ORB-14849] A before-landing review already published its PR and
    // promoted the task, so an evidence-only verdict is not held in progress:
    // like any other verdict but an approve, it leaves the PR open.
    let holds_evidence = context
        .admission
        .as_ref()
        .is_none_or(|admission| !admission.gates_landing());
    if !certificate.verdict.passed() {
        for task_id in context.task_ids.iter().filter(|_| holds_evidence) {
            if let Some(hold) =
                super::super::evidence::evidence_hold(runtime, task_id)?.filter(|hold| {
                    hold.schema_version == 1
                        && super::super::evidence::evidence_only(&certificate, &hold.requirements)
                        && hold.attempt_id == certificate.attempt_id
                        && hold.candidate == certificate.final_candidate
                        && hold.task_meaning_digest == certificate.task_meaning_digest
                })
            {
                runtime.apply_task_automation_update(task_id, TaskAutomationUpdate {
                    expected_status: Some(orbit_types::task::TaskStatus::InProgress),
                    status: Some(orbit_types::task::TaskStatus::InProgress),
                    status_event: Some("review_awaiting_evidence".into()),
                    status_note: Some(format!(
                        "run={}; candidate={}; awaiting named external checks; receipt queues a fresh review",
                        context.run_id, hold.candidate.commit,
                    )),
                    ..Default::default()
                })?;
                return Ok(Settled::AwaitingEvidence(Box::new(hold)));
            }
        }
        if !certificate.baseline_red.is_empty() {
            return Ok(Settled::BaselineRed(Box::new(certificate)));
        }
        return Ok(Settled::Blocked(Box::new(certificate)));
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
        "handoff_evidence": null,
    })
}
