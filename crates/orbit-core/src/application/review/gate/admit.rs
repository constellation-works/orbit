//! Admit a fresh reviewer for the committed, base-synchronized candidate.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_engine::review_gate::candidate_identity;
use orbit_store::contracts::ReviewReserveRequest;
use orbit_types::identity::Crew;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, ReviewAdmission,
    ReviewManifest, ReviewReservation, ReviewerIdentity,
};
use serde_json::{Value, json};

use super::super::REVIEW_AUDIT;
use super::super::admission::run_review_admission;
use crate::OrbitRuntime;
use crate::runtime::engine::crew::enforce_crew_allowlist;

use super::context::{GateContext, admitted_run_id, not_applicable};
use super::judgement::write_artifact;

/// Admit a reviewer for the committed, base-synchronized candidate.
///
/// Output always carries `applies`; when the gate does not apply the pipeline
/// continues to publish without a review. When it applies, the output pins
/// the candidate, names the reviewer, and records the attempt the settle
/// step must close.
pub(crate) fn review_gate_admit(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let failed = |message: String| DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message,
    };
    // A run without a captured review admission predates the policy or was
    // never a delivery submission: it keeps the pre-existing behavior and
    // never loads tasks or Git state for a gate that cannot apply.
    // Prefer the dispatcher-injected `run_id` (the admitted job) over
    // `job_run_id`, which a caller may spell as a stable worktree token that
    // has no run record [ORB-11520].
    let run_id = admitted_run_id(input).map_err(|error| failed(error.to_string()))?;
    let Some(admission) =
        run_review_admission(runtime, &run_id).map_err(|error| failed(error.to_string()))?
    else {
        return Ok(not_applicable("review_admission_missing", None));
    };
    if !admission.gates_pr() {
        let reason = match admission.timing {
            orbit_types::workflow::ReviewTiming::None => "review_policy_none",
            orbit_types::workflow::ReviewTiming::AfterLanding => "review_policy_after_landing",
            orbit_types::workflow::ReviewTiming::BeforePr => unreachable!("gates_pr"),
        };
        return Ok(not_applicable(reason, Some(&admission)));
    }
    if input
        .get("skipped_no_diff_expected")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        // A checked no-diff exemption delivers no code: nothing to review and
        // nothing that could later count as code coverage.
        return Ok(not_applicable("no_diff_exemption", Some(&admission)));
    }
    if input.get("mode").and_then(Value::as_str) == Some("local") {
        // V1 rejects `before-pr` on a local-only route instead of changing
        // what the policy means; a pipeline may learn its route late.
        return Err(failed(
            "review_policy_local_route_refused: this run carries a before-pr review admission \
             but delivers locally; ship through the PR route or choose none/after-landing"
                .to_string(),
        ));
    }

    let context = GateContext::load(runtime, input, Some(admission.clone()))
        .map_err(|error| failed(error.to_string()))?;
    let outcome = admit(runtime, &context, &admission);
    let audit_args = json!({
        "phase": "admit",
        "run_id": context.run_id,
        "task_ids": context.task_ids,
        "reviewer_crew": admission.crew,
        "outcome": outcome.as_ref().map(|value| value["decision"].clone()).unwrap_or(json!("refused")),
        "recorded_at": Utc::now().to_rfc3339(),
    });
    runtime
        .record_pipeline_audit(
            REVIEW_AUDIT,
            Some(&context.run_id),
            Some("system"),
            if outcome.is_ok() {
                AuditEventStatus::Success
            } else {
                AuditEventStatus::Failure
            },
            audit_args,
            outcome.as_ref().err().map(ToString::to_string),
        )
        .map_err(|error| failed(error.to_string()))?;
    outcome.map_err(|error| failed(error.to_string()))
}

fn admit(
    runtime: &OrbitRuntime,
    context: &GateContext,
    admission: &ReviewAdmission,
) -> Result<Value, OrbitError> {
    let crew = resolve_reviewer_crew(runtime, admission, context)?;
    let candidate = candidate_identity(&context.workspace_path, &context.base_sha()?)?;
    if candidate.commits.is_empty() {
        return Err(OrbitError::Execution(format!(
            "review_gate_admit: candidate {} adds no commits over base {}; nothing to review",
            candidate.head.commit, candidate.base.commit
        )));
    }
    let (task_digests, task_meaning_digest) = &context.task_digests;

    let store = runtime.review_store()?;
    let lineage_key = context.lineage_key();
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
            return Err(OrbitError::Execution(format!(
                "review_budget_exhausted: {reason} for lineage '{lineage_key}' (reviewer starts \
                 {}/{}, repair cycles {}/{}, {}s of {}s); a recorded decision must reset or \
                 re-scope this candidate before another review",
                consumed.reviewer_starts,
                ledger.budget.reviewer_starts,
                consumed.repair_cycles,
                ledger.budget.repair_cycles,
                consumed.seconds,
                u64::from(ledger.budget.minutes) * 60
            )));
        }
    };

    let manifest = ReviewManifest {
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: attempt.attempt_id.clone(),
        lineage_key: lineage_key.clone(),
        task_ids: context.task_ids.clone(),
        task_digests: task_digests.clone(),
        task_meaning_digest: task_meaning_digest.clone(),
        repository: context.repository.clone(),
        base: candidate.base.clone(),
        candidate: candidate.head.clone(),
        implementation_commits: candidate.commits.clone(),
        implementer_summaries: context
            .tasks
            .iter()
            .map(|task| (task.id.to_string(), task.execution_summary.clone()))
            .collect(),
        reviewer_crew: crew.name.clone(),
        contract_version: REVIEW_CONTRACT_VERSION,
        policy_version: admission.policy_version,
        budget: ledger.budget,
        remaining: ledger.remaining_at(now),
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

    Ok(json!({
        "applies": true,
        "decision": if resumed { "resumed" } else { "admitted" },
        "first_task_id": context.task_ids[0],
        "attempt_id": attempt.attempt_id,
        "attempt_index": attempt.index,
        "lineage_key": lineage_key,
        "timing": admission.timing.as_str(),
        "timing_source": admission.timing_source,
        "reviewer": reviewer_json(&crew, &admission.crew_source),
        "base_sha": candidate.base.commit,
        "base_tree": candidate.base.tree,
        "head_sha": candidate.head.commit,
        "head_tree": candidate.head.tree,
        "implementation_commit_count": candidate.commits.len(),
        "task_meaning_digest": manifest.task_meaning_digest,
        "task_selectors": selectors,
        "manifest_artifact": REVIEW_MANIFEST_ARTIFACT,
        "report_artifact": REVIEW_REPORT_ARTIFACT,
        "budget": ledger.budget,
        "remaining": ledger.remaining_at(now),
        "started_at": attempt.started_at.to_rfc3339(),
    }))
}

/// The reviewer crew must be explicitly configured, resolvable on this host,
/// and inside the run's crew allowlist. It is never inferred from the
/// implementer; refusal escalates rather than substituting.
fn resolve_reviewer_crew(
    runtime: &OrbitRuntime,
    admission: &ReviewAdmission,
    context: &GateContext,
) -> Result<Crew, OrbitError> {
    let name = admission.crew.as_deref().ok_or_else(|| {
        OrbitError::CapabilityDenied(
            "review_crew_unconfigured: before-pr review needs an explicitly configured \
             operation.review_crew; automatic review never inherits the implementer's crew"
                .to_string(),
        )
    })?;
    let crew = runtime
        .resolve_crew_for_task(Some(name), None)
        .map_err(|error| {
            OrbitError::CapabilityDenied(format!(
                "review_crew_unavailable: configured review crew '{name}' cannot be resolved on \
                 this host: {error}"
            ))
        })?;
    let run = runtime.get_job_run_backend(&context.run_id)?;
    let run_input = run.and_then(|run| run.input).unwrap_or(Value::Null);
    let allowlist = runtime.crew_allowlist_from_input(&run_input)?;
    enforce_crew_allowlist(allowlist.as_ref(), &crew, "the configured review crew").map_err(
        |error| {
            OrbitError::CapabilityDenied(format!(
                "review_crew_excluded: {error}; the gate escalates rather than substituting a \
                 crew the run's window excluded"
            ))
        },
    )?;
    Ok(crew)
}

fn reviewer_json(crew: &Crew, source: &str) -> Value {
    json!({
        "crew": crew.name,
        "crew_source": source,
        "provider": crew.assignment.provider,
        "model": crew.assignment.model,
        "reasoning_effort": crew.assignment.effort,
    })
}

pub(super) fn reviewer_identity(
    runtime: &OrbitRuntime,
    context: &GateContext,
    admission_output: &Value,
) -> Result<ReviewerIdentity, OrbitError> {
    let reviewer = admission_output
        .get("reviewer")
        .cloned()
        .unwrap_or(Value::Null);
    let field = |key: &str| {
        reviewer
            .get(key)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "review_gate_settle requires admission.reviewer.{key}"
                ))
            })
    };
    let implementer_model = runtime
        .get_job_run_backend(&context.run_id)?
        .and_then(|run| run.crew_model)
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty());
    let model = field("model")?;
    Ok(ReviewerIdentity {
        crew: field("crew")?,
        provider: field("provider")?,
        same_model_as_implementer: implementer_model.as_deref() == Some(model.as_str()),
        model,
        reasoning_effort: reviewer
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        implementer_model,
    })
}
