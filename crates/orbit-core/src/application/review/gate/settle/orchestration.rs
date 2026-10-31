use std::time::Duration;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::review_gate::{
    ReplayBudget, candidate_identity_at, committed_paths, uncommitted_paths,
};
use orbit_engine::{DispatchError, ReviewReleaseRequest};
use orbit_store::contracts::ReviewSettlement;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    REVIEW_ABANDONED_MARKER, REVIEW_CONTRACT_VERSION, ReviewAttemptState, ReviewCertificate,
    ReviewGateAdmissionOutput, ReviewGateAdmitted, ReviewGateDecision, ReviewGateRefusal,
    ReviewerIdentity,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::review::REVIEW_AUDIT;
use crate::application::review::gate::admit::reviewer_identity;
use crate::application::review::gate::baseline;
use crate::application::review::gate::context::GateContext;
use crate::application::review::gate::host_gates::admitted_host_gates;
use crate::application::review::gate::judgement::{Judgement, repair_author_label, write_artifact};
use crate::application::review::gate::owed::{
    defers_linux_codeql, owed_evidence, owed_hold_received, split_owner_owed,
};
use crate::application::review::gate::release::release_review_attempt;

use super::model::Settled;
use super::{candidate, output, publication};

/// Close the admitted attempt with an honest verdict.
///
/// An accept returns the reviewed head and base the PR steps must recheck.
/// With the reviewer's fixes committed it also reports `reviewer_fixed`, so
/// the pipeline reruns owner validation and the ownership check on that head
/// before publishing, and the PR body's "Review fixes" section [ORB-13989].
/// A reviewer that exited cleanly with only its initial provisional report
/// abandoned the review: the attempt is released, not settled, so it does not
/// spend the candidate's one review, and the refusal leads with
/// `review_abandoned:` [ORB-15130]. An evidence-only verdict holds delivery
/// without recovery. Other verdicts
/// refuse the step — a settled verdict is not retried and
/// never goes back to the implementer — so the pipeline's failure handoff
/// preserves the candidate and blocks the task with the escalation.
pub(crate) fn review_gate_settle(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    // The admission step's output, parsed once; `null` when it recorded none.
    let admission_output = input
        .get("admission")
        .filter(|admission| !admission.is_null())
        .map(ReviewGateAdmissionOutput::from_value)
        .transpose()
        .map_err(|error| {
            DispatchError::action_failed(
                action,
                format!("review_gate_settle requires a readable admission ({error})"),
            )
        })?;
    let admitted = match admission_output {
        Some(ReviewGateAdmissionOutput::Admitted(admitted)) => admitted,
        Some(ReviewGateAdmissionOutput::PreflightPassed { .. }) => {
            return Err(DispatchError::action_failed(
                action,
                "review_gate_settle requires admission.attempt_id; a budget preflight admits \
                 no attempt"
                    .to_string(),
            ));
        }
        Some(ReviewGateAdmissionOutput::NotApplicable(not_applicable)) => {
            let mut output = output::not_required(Some(not_applicable.reason));
            if let Some(receipt) = not_applicable.selective_receipt {
                output["selective_selection_head_sha"] = json!(receipt.head_sha);
                output["selective_receipt"] = json!(receipt);
            }
            return Ok(output);
        }
        None => return Ok(output::not_required(None)),
    };
    let mut settle_input = input.clone();
    if let Some(object) = settle_input.as_object_mut() {
        object.insert("base_sha".to_string(), json!(admitted.base_sha));
    }
    let mut context = GateContext::load(runtime, &settle_input, None)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    if context.admission.is_none() {
        return Err(DispatchError::action_failed(
            action,
            "review_gate_stale: the run no longer carries a review admission".to_string(),
        ));
    }
    if let Some(selection) = admitted.selective_receipt.as_deref()
        && selection.requires_review()
    {
        let captured = context.admission.as_mut().ok_or_else(|| {
            DispatchError::action_failed(
                action,
                "review_gate_stale: missing captured admission".to_string(),
            )
        })?;
        if captured
            .selective_policy()
            .is_none_or(|policy| policy.digest != selection.policy_digest)
        {
            return Err(DispatchError::action_failed(
                action,
                "selection_stale: admitted selection does not bind captured owner policy"
                    .to_string(),
            ));
        }
        // This is only the effective settlement layer. The immutable run
        // admission remains unchanged, including its captured model/policy.
        if !captured.gates_landing() {
            captured.timing = orbit_types::workflow::ReviewTiming::BeforeLanding;
            captured.timing_source = "selective_before_landing".to_string();
        }
    }
    if admitted.attempt_id.trim().is_empty() {
        return Err(DispatchError::action_failed(
            action,
            "review_gate_settle requires admission.attempt_id".to_string(),
        ));
    }
    let attempt_id = admitted.attempt_id.clone();
    let reviewer = reviewer_identity(runtime, &context, &admitted.reviewer)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;

    let outcome = settle(runtime, &mut context, &attempt_id, reviewer, &admitted);
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
        Ok(Settled::Abandoned) => (
            AuditEventStatus::Success,
            json!({"gate": "abandoned", "released": true}),
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
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;

    match outcome {
        Ok(Settled::Passed(value)) => Ok(value),
        Ok(Settled::AwaitingEvidence(hold)) => Err(DispatchError::ReviewEvidenceHold(hold)),
        Ok(Settled::Blocked(certificate)) => Err(DispatchError::ReviewGateRefused {
            action: action.to_string(),
            refusal: ReviewGateRefusal::Blocked,
            message: format!(
                "review_gate_blocked: verdict {} ({}); {} finding(s) recorded; {}",
                certificate.verdict.as_str(),
                certificate
                    .escalation
                    .as_deref()
                    .unwrap_or("no escalation reason recorded"),
                certificate.findings.len(),
                output::candidate_standing(&admitted)
            ),
        }),
        // Typed `[baseline_red]`, so the failure handoff keeps the candidate
        // and holds the task until the base passes [ORB-14434].
        Ok(Settled::BaselineRed(certificate)) => Err(DispatchError::DeterministicActionRefused {
            action: action.to_string(),
            message: baseline::baseline_red_refusal(&certificate.baseline_red, &attempt_id)
                .unwrap_or_default(),
        }),
        Ok(Settled::Abandoned) => Err(DispatchError::ReviewGateRefused {
            action: action.to_string(),
            refusal: ReviewGateRefusal::Abandoned,
            message: format!(
                "{REVIEW_ABANDONED_MARKER} the reviewer exited successfully but its only report \
                 is the initial provisional one (verdict incomplete, no escalation, finding or \
                 validation record, never updated); attempt {attempt_id} was released without a \
                 verdict, so the candidate's review is not spent and a reviewer is admitted again \
                 within its remaining minutes; {}",
                output::candidate_standing(&admitted)
            ),
        }),
        Err(error) => Err(DispatchError::action_failed(action, error.to_string())),
    }
}

fn settle(
    runtime: &OrbitRuntime,
    context: &mut GateContext,
    attempt_id: &str,
    reviewer: ReviewerIdentity,
    admitted: &ReviewGateAdmitted,
) -> Result<Settled, OrbitError> {
    let store = runtime.review_store()?;
    // The admission names the lineage it reserved in; a resumed run reuses
    // that admission, so settlement never re-derives a different key.
    let lineage_key = Some(admitted.lineage_key.as_str())
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
        return candidate::reconcile_settled(
            runtime,
            context,
            certificate,
            admitted.selective_receipt.as_deref(),
        );
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
    let held = if admitted.decision == ReviewGateDecision::EvidenceReceived {
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
        let held_attempt = admitted.held_attempt_id.as_deref();
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
    let admitted_selectors = &admitted.task_selectors;
    judgement.check_task_meaning(context, &attempt, admitted_selectors)?;
    // [ORB-15130] A reviewer that ended its session with nothing but the
    // provisional report it persisted first produced no verdict. Settling it
    // would count it as the candidate's one review, so the attempt is released
    // and the step refuses with a typed reason. A reviewer timeout fails the
    // reviewer step before settlement runs, so only a clean exit reaches this.
    // Anything the reviewer changed or revised is a real review, however
    // incomplete.
    if held.is_none()
        && recorded.is_none()
        && committed_repair.is_none()
        && judgement.abandoned_placeholder()
        && uncommitted_paths(&context.workspace_path)?.is_empty()
    {
        release_review_attempt(
            runtime,
            &ReviewReleaseRequest {
                run_id: context.run_id.clone(),
                lineage_key: lineage_key.clone(),
                attempt_id: attempt_id.to_string(),
            },
        )?;
        return Ok(Settled::Abandoned);
    }
    let repair = match committed_repair {
        Some(commit) => {
            let paths = committed_paths(&context.workspace_path, &commit.commit)?;
            judgement.adopt_repairs(runtime, context, &paths, admitted_selectors)?;
            Some(commit)
        }
        // A settled ledger never charged a repair that was not committed.
        None if recorded.is_some() => None,
        None => judgement.commit_repairs(runtime, context, &reviewer, &attempt)?,
    };

    let validation_scope =
        candidate::validation_scope(context, &reviewed.commits, repair.as_ref(), &[])?;
    let final_candidate = match &repair {
        Some(commit) => SourceRevision {
            commit: commit.commit.clone(),
            tree: commit.tree.clone(),
        },
        None => reviewed.head.clone(),
    };
    let carry = match context.task_ids.as_slice() {
        [task_id] => crate::application::review::evidence::evidence_carry(
            runtime,
            task_id,
            &context.workspace_path,
            &reviewed.base,
            &final_candidate,
        )?,
        _ => crate::application::review::evidence::EvidenceCarry::None,
    };
    // Checks a host-evidence rule owes for what this candidate changed are
    // required whatever the reviewer reported.
    // [ORB-15630] Linux CodeQL a macOS host cannot run is the owner's and
    // the pull request's hosted CI's on a before-landing review, when its
    // rule names the hosted check that runs it [ORB-15677].
    let defers = defers_linux_codeql(runtime, context);
    let (owed, owner_owed) = match &held {
        Some((_, certificate)) => (
            certificate.owed_evidence.clone(),
            certificate.owner_owed_evidence.clone(),
        ),
        None => {
            let mut owed = owed_evidence(runtime, context, reviewed.commits.iter().chain(&repair))?;
            if defers {
                judgement.add_named_linux_sandbox_evidence(&mut owed);
            }
            split_owner_owed(owed, defers)
        }
    };
    judgement.require_owed_evidence(&owed);
    if held.is_none() {
        judgement.defer_to_owner(&owner_owed, defers);
        // [ORB-15630] The claim host ran the owner's required gates on the
        // reviewed candidate outside the agent sandbox; its results decide
        // those checks, not the reviewer's sandboxed records.
        let gates = admitted_host_gates(admitted, &reviewed.head.commit);
        judgement.adopt_host_gates(&gates, &reviewed.base.commit, repair.is_some());
    }
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
    // the review incomplete. [ORB-15122] A check the host passes on the
    // candidate counts as passed; one it fails while the base passes rejects.
    // [ORB-15546] One that fails only tests the base fails too is known red
    // and continues, its record still failed.
    // [ORB-15328] Each claim's host runs get one review budget of wall clock,
    // with the base replay's share reserved.
    let baseline_red = judgement.verify_baseline_claims(
        runtime,
        context,
        &reviewed.base,
        &final_candidate,
        &context.base_ref(),
        &validation_scope,
        repair.as_ref(),
        ReplayBudget::new(Duration::from_secs(
            u64::from(ledger.budget.minutes).saturating_mul(60),
        )),
    )?;
    judgement.accept_on_host_evidence(repair.as_ref(), &validation_scope, !baseline_red.is_empty());
    // [ORB-14616] Every file a control mutated must come back byte-identical
    // in the final candidate. [ORB-16447] One this attempt's own repair
    // changed comes back to the repaired blob its record declares.
    let unrestored = judgement.unrestored_mutation(
        &context.workspace_path,
        &reviewed.head.commit,
        &final_candidate.commit,
        repair.as_ref(),
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
        host_overrides: judgement.host_overrides.clone(),
        known_red: judgement.known_red.clone(),
        host_evidence: judgement.host_evidence.clone(),
        owed_evidence: owed,
        owner_owed_evidence: judgement.owner_owed.clone(),
        host_validation: judgement.host_validation.clone(),
        resumed_hold_attempt: held.map(|(hold, _)| hold.attempt_id),
        issued_at: now,
    };
    if crate::application::review::evidence::evidence_only(
        &certificate,
        &judgement.external_evidence,
    ) {
        let mut hold = orbit_types::workflow::ReviewEvidenceHold {
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
            published_ref: if context.claimed {
                candidate::publish_held_candidate(context, &certificate.final_candidate.commit)
            } else {
                None
            },
            candidate_carry: None,
        };
        if context.claimed && hold.published_ref.is_none() {
            hold.candidate_carry =
                candidate::carry_evidence_candidate(runtime, context, &hold.candidate.commit)?;
        }
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
    output::validate_selection(&certificate, admitted.selective_receipt.as_deref())?;
    store.review_certificate_record(&context.workspace_id, &certificate)?;
    publication::publish_certificate(runtime, context, &certificate)?;
    if certificate.verdict.passed() {
        baseline::record_known_red(runtime, &context.task_ids, &certificate.known_red)?;
    }
    output::settled_outcome(
        runtime,
        context,
        certificate,
        admitted.selective_receipt.as_deref(),
    )
}
