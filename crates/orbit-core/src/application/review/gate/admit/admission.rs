//! Reserve a review attempt and publish its bound manifest.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::review_gate::candidate_identity;
use orbit_store::contracts::ReviewReserveRequest;
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, ReviewAdmission,
    ReviewGateAdmissionOutput, ReviewGateAdmitted, ReviewGateDecision, ReviewManifest,
    ReviewReservation,
};

use super::super::super::evidence::{
    EvidenceCarry, carried_external_evidence, evidence_carry,
    satisfied_external_evidence as satisfied_external_evidence_on,
};
use super::super::context::GateContext;
use super::super::host_gates::recorded_host_gates;
use super::super::judgement::write_artifact;
use super::super::owed::{
    defers_linux_codeql, owed_evidence, owed_hold_received, split_owner_owed,
};
use super::super::release::release_abandoned;
use super::holds::CompletionRebase;
use super::reviewer;
use crate::OrbitRuntime;

pub(super) fn admit(
    runtime: &OrbitRuntime,
    context: &GateContext,
    admission: &ReviewAdmission,
    rebase: Option<&CompletionRebase>,
    host_step: Option<&str>,
) -> Result<ReviewGateAdmissionOutput, OrbitError> {
    let required_validation_commands = admission
        .required_validation_commands
        .as_ref()
        .ok_or_else(|| {
            OrbitError::CapabilityDenied(
                "review_validation_contract_missing: this admitted run predates the captured host required-check list; dispatch a fresh delivery run after upgrading the workspace".into(),
            )
        })?;
    let candidate = candidate_identity(&context.workspace_path, &context.base_sha()?)?;
    if let Some(rebase) = rebase
        && candidate.head.commit != rebase.head_sha
    {
        return Err(OrbitError::Execution(format!(
            "review_gate_stale: the worktree head {} is not the rebased head {} completion \
             recorded",
            candidate.head.commit, rebase.head_sha
        )));
    }
    if candidate.commits.is_empty() {
        return Err(OrbitError::Execution(format!(
            "review_gate_admit: candidate {} adds no commits over base {}; nothing to review",
            candidate.head.commit, candidate.base.commit
        )));
    }
    // A held review whose owed evidence arrived for this exact candidate
    // settles under its original reviewer; no new reviewer is started.
    let received = match rebase {
        None => owed_hold_received(runtime, context, &candidate)?,
        Some(_) => None,
    };
    let reviewer = match &received {
        Some((_, certificate)) => reviewer::held_reviewer(&certificate.reviewer),
        None => reviewer::resolve_reviewer_crew(
            runtime,
            admission,
            context,
            &candidate.base.commit,
            &candidate.head.commit,
        )?,
    };
    // [ORB-15630] Linux CodeQL a macOS host cannot run, whose rule names the
    // pull request's hosted check that runs it [ORB-15677], is owed to the
    // owner and that check, not held by this before-landing
    // review; the host's own runs of the owner's gates decide those checks.
    let (owed_external_evidence, owner_owed_evidence) = split_owner_owed(
        owed_evidence(runtime, context, &candidate.commits)?,
        defers_linux_codeql(runtime, context),
    );
    let host_validation = match (host_step, rebase) {
        (Some(step), None) => {
            let trusted = required_validation_commands
                .iter()
                .chain(&admission.baseline_commands)
                .cloned()
                .collect::<Vec<_>>();
            recorded_host_gates(runtime, context, step, &candidate.head.commit, &trusted)?
        }
        _ => Vec::new(),
    };
    let (task_digests, task_meaning_digest) = &context.task_digests;
    let carry = match context.task_ids.as_slice() {
        [task_id] => evidence_carry(
            runtime,
            task_id,
            &context.workspace_path,
            &candidate.base,
            &candidate.head,
        )?,
        _ => EvidenceCarry::None,
    };

    let store = runtime.review_store()?;
    let lineage_key = context.lineage_key();
    release_abandoned(
        runtime,
        &context.workspace_id,
        &lineage_key,
        &context.run_id,
    )?;
    let now = Utc::now();
    let (reservation, ledger) = store.review_reserve(
        &context.workspace_id,
        &ReviewReserveRequest {
            lineage_key: &lineage_key,
            task_ids: &context.task_ids,
            run_id: &context.run_id,
            task_meaning_digest,
            candidate: &candidate.head,
            budget: admission.budget,
            now,
        },
    )?;
    let (attempt, resumed) = match reservation {
        ReviewReservation::Reserved { attempt } => (attempt, false),
        ReviewReservation::Resumed { attempt } => (attempt, true),
        ReviewReservation::Exhausted { reason, consumed } => {
            return Err(OrbitError::CapabilityDenied(format!(
                "review_budget_exhausted: {reason} for candidate {} in lineage '{lineage_key}' \
                 ({}s of {}s); each candidate gets one review — {}",
                candidate.head.commit,
                consumed.seconds,
                u64::from(ledger.budget.minutes) * 60,
                reviewer::budget_recovery(context, &lineage_key)
            )));
        }
    };

    let previous_report = runtime
        .get_task_artifact(&context.task_ids[0], REVIEW_REPORT_ARTIFACT)?
        .and_then(|artifact| orbit_types::workflow::ReviewReport::parse(&artifact.content).ok());
    let mut satisfied_external_evidence = carried_external_evidence(
        runtime,
        &context.task_ids[0],
        &candidate.head,
        carry.carried(),
    )?;
    satisfied_external_evidence.extend(satisfied_external_evidence_on(
        runtime,
        &context.task_ids[0],
        &candidate.head,
    )?);
    // [granted-tools §4] Every unapplied proposal goes in front of this
    // reviewer, bound to the bytes read here; settlement applies only these.
    // A claimed leaf reads none: a granted task runs only on its owner, and
    // claimed-worker evidence cannot carry a proposal.
    let mut mutation_proposals = Vec::new();
    for task_id in context.task_ids.iter().filter(|_| !context.claimed) {
        mutation_proposals.extend(runtime.unapplied_mutation_proposals(task_id)?);
    }
    let manifest = ReviewManifest {
        mutation_proposals,
        satisfied_external_evidence,
        evidence_carried: carry.carried().cloned(),
        owed_external_evidence,
        owner_owed_evidence,
        host_validation,
        previous_report,
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: attempt.attempt_id.clone(),
        lineage_key: lineage_key.clone(),
        task_ids: context.task_ids.clone(),
        task_digests: task_digests.clone(),
        required_validation_commands: Some(required_validation_commands.clone()),
        baseline_commands: admission.baseline_commands.clone(),
        task_meaning_digest: task_meaning_digest.clone(),
        repository: context.repository.clone(),
        base: candidate.base.clone(),
        candidate: candidate.head.clone(),
        implementation_commits: candidate.commits.clone(),
        implementer_summaries: context.implementer_summaries(runtime)?,
        reviewer_crew: reviewer.crew.clone(),
        contract_version: REVIEW_CONTRACT_VERSION,
        policy_version: admission.policy_version,
        budget: ledger.budget,
        remaining: ledger.remaining_for(&candidate.head, task_meaning_digest, now),
        issued_at: now,
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| OrbitError::Execution(format!("serialize review manifest: {error}")))?;
    let selectors = context
        .tasks
        .iter()
        .map(|task| (task.id.to_string(), task.context_files.clone()))
        .collect::<BTreeMap<_, _>>();
    for task in &context.tasks {
        write_artifact(
            runtime,
            &task.id,
            &context.run_id,
            REVIEW_MANIFEST_ARTIFACT,
            &manifest_bytes,
        )?;
    }

    let decision = match (&received, resumed) {
        (Some(_), _) => ReviewGateDecision::EvidenceReceived,
        (None, true) => ReviewGateDecision::Resumed,
        (None, false) => ReviewGateDecision::Admitted,
    };
    Ok(ReviewGateAdmissionOutput::Admitted(Box::new(
        ReviewGateAdmitted {
            decision,
            held_attempt_id: received.map(|(hold, _)| hold.attempt_id),
            first_task_id: context.task_ids[0].clone(),
            attempt_id: attempt.attempt_id,
            attempt_index: attempt.index,
            lineage_key,
            timing: admission.timing,
            timing_source: admission.timing_source.clone(),
            reviewer,
            owed_evidence: manifest.owed_external_evidence,
            owner_owed_evidence: manifest.owner_owed_evidence,
            host_validation: manifest.host_validation,
            base_sha: candidate.base.commit,
            base_tree: candidate.base.tree,
            head_sha: candidate.head.commit,
            head_tree: candidate.head.tree,
            implementation_commit_count: candidate.commits.len(),
            task_meaning_digest: manifest.task_meaning_digest,
            task_selectors: selectors,
            evidence_carry: carry.to_json(),
            manifest_artifact: REVIEW_MANIFEST_ARTIFACT.to_string(),
            report_artifact: REVIEW_REPORT_ARTIFACT.to_string(),
            budget: ledger.budget,
            remaining: manifest.remaining,
            started_at: attempt.started_at,
            selective_receipt: None,
        },
    )))
}
