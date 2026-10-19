//! Admit a fresh reviewer for the committed, base-synchronized candidate.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_engine::review_gate::candidate_identity;
use orbit_store::contracts::ReviewReserveRequest;
use orbit_types::identity::Crew;
use orbit_types::task::SKIP_REVIEW_TAG;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, ReviewAdmission,
    ReviewGateAdmissionOutput, ReviewGateAdmitted, ReviewGateDecision, ReviewGateRefusal,
    ReviewGateReviewer, ReviewManifest, ReviewReservation, ReviewerIdentity,
};
use serde_json::{Value, json};

use super::super::REVIEW_AUDIT;
use super::super::admission::run_review_admission;
use super::super::evidence::{
    EvidenceCarry, carried_external_evidence, evidence_carry,
    satisfied_external_evidence as satisfied_external_evidence_on,
};
use crate::OrbitRuntime;
use crate::application::job::crew_pools::seeded_crew_ticket;
use crate::application::review::crew::pool_source;
use crate::application::task::light_delivery::{
    carries, light_delivery_decision, record_light_delivery,
};
use crate::runtime::engine::crew::enforce_crew_allowlist;

use super::context::{GateContext, admitted_run_id, not_applicable};
use super::host_gates::{HOST_VALIDATION_STEP_INPUT, recorded_host_gates};
use super::judgement::write_artifact;
use super::owed::{defers_linux_codeql, owed_evidence, owed_hold_received, split_owner_owed};
use super::release::release_abandoned;

/// Admit a reviewer for the committed, base-synchronized candidate.
///
/// Output is a [`ReviewGateAdmissionOutput`], which always carries
/// `applies`; when the gate does not apply the pipeline continues to publish
/// without a review. When it applies, the output pins the candidate, names
/// the reviewer, and records the attempt the settle step must close.
pub(crate) fn review_gate_admit(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let output = admission_output(runtime, action, input)?;
    serde_json::to_value(&output).map_err(|error| {
        DispatchError::action_failed(action, format!("serialize review admission: {error}"))
    })
}

fn admission_output(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<ReviewGateAdmissionOutput, DispatchError> {
    let refused = |message: String| DispatchError::DeterministicActionRefused {
        action: action.to_string(),
        message,
    };
    // A capability or budget refusal is a decision a retry would only
    // repeat; everything else (Git, store contention) may be transient.
    let refused_or_failed = |error: OrbitError| match error {
        OrbitError::CapabilityDenied(message) => refused(message),
        other => DispatchError::action_failed(action, other.to_string()),
    };
    // A run without a captured review admission predates the policy or was
    // never a delivery submission: it keeps the pre-existing behavior and
    // never loads tasks or Git state for a gate that cannot apply.
    // Prefer the dispatcher-injected `run_id` (the admitted job) over
    // `job_run_id`, which a caller may spell as a stable worktree token that
    // has no run record [ORB-11520].
    let run_id = admitted_run_id(input)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    let rebase = match input.get("re_review_after").and_then(Value::as_str) {
        Some(step_id) => match completion_rebase(runtime, &run_id, step_id, input)
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?
        {
            Some(rebase) => Some(rebase),
            None => return Ok(not_applicable("re_review_not_required", None)),
        },
        None => None,
    };
    let Some(admission) = run_review_admission(runtime, &run_id)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?
    else {
        if rebase.is_some() {
            return Err(refused(
                "review_gate_stale: completion rebased a reviewed head but the run no longer \
                 carries a review admission"
                    .to_string(),
            ));
        }
        return Ok(not_applicable("review_admission_missing", None));
    };
    if rebase.is_some() && !admission.gates_merge() {
        return Err(refused(
            "review_gate_stale: completion rebased a reviewed head but the run's review \
             admission no longer gates the merge"
                .to_string(),
        ));
    }
    let preflight = input
        .get("preflight")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // [ORB-14849] Which review this step is: the published PR's
    // (`before_landing`), the pre-push gate's, or — for a completion re-review
    // or the budget preflight — whichever one the run captured.
    let landing_step = input
        .get("before_landing")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let applies = if rebase.is_some() || preflight {
        admission.gates_merge()
    } else if landing_step {
        admission.gates_landing()
    } else {
        admission.gates_pr()
    };
    if !applies {
        use orbit_types::workflow::ReviewTiming;
        let reason = match admission.timing {
            ReviewTiming::None if landing_step => "review_before_landing_off",
            ReviewTiming::None => "review_before_pr_off",
            // The other layer reviews this run.
            ReviewTiming::BeforePr => "reviewed_before_pr",
            ReviewTiming::BeforeLanding => "review_before_landing",
            // A run captured under the retired `after-landing` policy value.
            ReviewTiming::AfterLanding => "review_policy_after_landing",
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
        // A review layer before landing on a local-only route is refused
        // rather than reinterpreted; a pipeline may learn its route late.
        let key = if admission.gates_landing() {
            "review.before_landing"
        } else {
            "review.before_pr"
        };
        return Err(refused(format!(
            "{}_local_route_refused: this run captured {key} on but delivers locally; ship \
             through the PR route or turn {key} off",
            key.replace('.', "_")
        )));
    }

    // A re-review pins the candidate to the base completion rebased onto.
    let mut admit_input = input.clone();
    if let (Some(rebase), Some(object)) = (&rebase, admit_input.as_object_mut()) {
        object.insert("base_sha".to_string(), json!(rebase.base_sha));
    }
    let context = GateContext::load(runtime, &admit_input, Some(admission.clone()))
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    // [ORB-16855] A candidate whose tasks all carry `skip-review` and that
    // changes only `workflow.light_delivery_paths` is published and landed
    // gated only by the hosted checks. The budget preflight runs before any
    // candidate exists, so it keeps its normal decision.
    if !preflight
        && light_review_skipped(runtime, &context)
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?
    {
        return Ok(not_applicable(SKIP_REVIEW_REASON, Some(&admission)));
    }
    let mut awaiting_evidence = false;
    let outcome = awaiting_external_evidence(runtime, &context).and_then(|awaiting| {
        awaiting_evidence = awaiting;
        if awaiting {
            Err(OrbitError::CapabilityDenied(
                "review_awaiting_evidence: named external checks have not arrived for the held \
                 candidate"
                    .into(),
            ))
        } else if preflight {
            preflight_budget(runtime, &context)
        } else {
            let host_step = input
                .get(HOST_VALIDATION_STEP_INPUT)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|step| !step.is_empty());
            admit(runtime, &context, &admission, rebase.as_ref(), host_step)
        }
    });
    let audit_args = json!({
        "phase": if preflight { "preflight" } else { "admit" },
        "run_id": context.run_id,
        "task_ids": context.task_ids,
        "reviewer_crew": admission.crew,
        "outcome": outcome.as_ref().map_or(json!("refused"), |output| json!(output.decision())),
        "recorded_at": Utc::now().to_rfc3339(),
    });
    runtime
        .record_pipeline_audit(
            REVIEW_AUDIT,
            Some(&context.run_id),
            Some("system"),
            match &outcome {
                Ok(_) => AuditEventStatus::Success,
                Err(OrbitError::CapabilityDenied(_)) => AuditEventStatus::Denied,
                Err(_) => AuditEventStatus::Failure,
            },
            audit_args,
            outcome.as_ref().err().map(ToString::to_string),
        )
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    outcome.map_err(|error| match error {
        OrbitError::CapabilityDenied(message) if awaiting_evidence => {
            DispatchError::ReviewGateRefused {
                action: action.to_string(),
                refusal: ReviewGateRefusal::AwaitingEvidence,
                message,
            }
        }
        other => refused_or_failed(other),
    })
}

/// Why a `skip-review` candidate within the light-delivery paths is not
/// reviewed [ORB-16855].
const SKIP_REVIEW_REASON: &str = "skip_review_light_delivery";

/// Judge the checked-out candidate on its pinned base for `skip-review`,
/// record the decision in each task's history, and say whether review is
/// skipped. A run none of whose tasks carry the tag judges nothing.
fn light_review_skipped(runtime: &OrbitRuntime, context: &GateContext) -> Result<bool, OrbitError> {
    if !context
        .tasks
        .iter()
        .any(|task| carries(task, SKIP_REVIEW_TAG))
    {
        return Ok(false);
    }
    let base = context.base_sha()?;
    let head = candidate_identity(&context.workspace_path, &base)?
        .head
        .commit;
    let Some(decision) = light_delivery_decision(
        runtime,
        &context.workspace_path,
        &base,
        &head,
        &context.tasks,
        SKIP_REVIEW_TAG,
    )?
    else {
        return Ok(false);
    };
    record_light_delivery(runtime, &context.task_ids, &decision)?;
    Ok(decision.applies())
}

/// Whether a task's held candidate is still waiting for named external
/// evidence: the hold matches its task meaning and tree, and not every
/// requirement has arrived.
fn awaiting_external_evidence(
    runtime: &OrbitRuntime,
    context: &GateContext,
) -> Result<bool, OrbitError> {
    for task in &context.tasks {
        if let Some(hold) = super::super::evidence::evidence_hold(runtime, &task.id)?
            && hold.task_meaning_digest == context.task_digests.1
            && orbit_engine::review_gate::revision(&context.workspace_path, "HEAD")?.tree
                == hold.candidate.tree
            && !super::super::evidence::evidence_ready(runtime, &task.id, &hold)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The head a completion step rebased onto a new base and left unpublished
/// for re-review, as that step recorded it for this run.
struct CompletionRebase {
    head_sha: String,
    base_sha: String,
}

/// Whether the completion step `step_id` asked for a re-review. Read from the
/// run's own recorded pipeline rather than a step template: the completion
/// step is skipped on review-only and no-diff routes, and a `when:` may not
/// read a skippable step's output.
fn completion_rebase(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_id: &str,
    input: &Value,
) -> Result<Option<CompletionRebase>, OrbitError> {
    // `--auto-fix` hands a repaired reviewed head to re-review too, without
    // completion authority.
    let completes = input.get("completion").and_then(Value::as_str) == Some("done")
        || input.get("auto_fix").and_then(Value::as_bool) == Some(true);
    let no_diff = input
        .get("skipped_no_diff_expected")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !completes || no_diff {
        return Ok(None);
    }
    let state = runtime.stores().jobs().read_run_state(run_id)?;
    let output = state
        .as_ref()
        .and_then(|state| state.pipeline.get(step_id))
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "re-review admission: run '{run_id}' recorded no `{step_id}` checkpoint"
            ))
        })?;
    if output.get("re_review_required").and_then(Value::as_bool) != Some(true) {
        return Ok(None);
    }
    let sha = |key: &str| {
        output["rebased"]
            .get(key)
            .and_then(Value::as_str)
            .filter(|sha| !sha.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                OrbitError::Execution(format!(
                    "re-review admission: `{step_id}` asked for a re-review without \
                     rebased.{key}"
                ))
            })
    };
    Ok(Some(CompletionRebase {
        head_sha: sha("head_sha")?,
        base_sha: sha("base_sha")?,
    }))
}

fn admit(
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
        Some((_, certificate)) => held_reviewer(&certificate.reviewer),
        None => {
            let (crew, source) =
                resolve_reviewer_crew(runtime, admission, context, &candidate.head.commit)?;
            admitted_reviewer(&crew, source)
        }
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
                budget_recovery(context, &lineage_key)
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
    let manifest = ReviewManifest {
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
        },
    )))
}

/// The reviewer crew must be explicitly configured, resolvable on this host,
/// and inside the run's crew allowlist. It is never inferred from the
/// implementer; refusal escalates rather than substituting.
///
/// A captured pool chooses one member per candidate [ORB-15195], preferring
/// one that did not implement the run's tasks; the draw is seeded by the run
/// and candidate head, so a resumed admission chooses the same reviewer.
/// Returns the crew with the provenance its reviewer record carries.
fn resolve_reviewer_crew(
    runtime: &OrbitRuntime,
    admission: &ReviewAdmission,
    context: &GateContext,
    head: &str,
) -> Result<(Crew, String), OrbitError> {
    let run = runtime.get_job_run_backend(&context.run_id)?;
    let run_input = run.and_then(|run| run.input).unwrap_or(Value::Null);
    let allowlist = runtime.crew_allowlist_from_input(&run_input)?;
    if !admission.crew_pool.is_empty() {
        let names = admission
            .crew_pool
            .iter()
            .map(|member| member.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let candidates = runtime
            .review_crew_candidates(&admission.crew_pool)
            .map_err(|error| {
                OrbitError::CapabilityDenied(format!(
                    "review_crew_unavailable: no crew of the configured review pool [{names}] \
                     can be resolved on this host: {error}"
                ))
            })?;
        let mut excluded = None;
        let permitted = candidates
            .into_iter()
            .filter(|candidate| {
                enforce_crew_allowlist(allowlist.as_ref(), &candidate.crew, "the review pool")
                    .map_err(|error| excluded.get_or_insert(error))
                    .is_ok()
            })
            .collect::<Vec<_>>();
        if permitted.is_empty() {
            let error = excluded.map_or_else(String::new, |error| error.to_string());
            return Err(OrbitError::CapabilityDenied(format!(
                "review_crew_excluded: {error}; the run's window excludes every crew of the \
                 configured review pool [{names}], and the gate escalates rather than \
                 substituting another crew"
            )));
        }
        let implementers = context
            .tasks
            .iter()
            .filter_map(|task| task.crew.clone())
            .chain(
                run_input
                    .get("crew")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
            .collect();
        let crew = runtime.draw_review_crew(
            permitted,
            &implementers,
            &mut seeded_crew_ticket(format!("review:{}:{head}", context.run_id)),
        )?;
        return Ok((crew, pool_source(&admission.crew_source)));
    }
    let name = admission.crew.as_deref().ok_or_else(|| {
        OrbitError::CapabilityDenied(
            "review_crew_unconfigured: before-PR review needs an explicitly configured \
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
    enforce_crew_allowlist(allowlist.as_ref(), &crew, "the configured review crew").map_err(
        |error| {
            OrbitError::CapabilityDenied(format!(
                "review_crew_excluded: {error}; the gate escalates rather than substituting a \
                 crew the run's window excluded"
            ))
        },
    )?;
    Ok((crew, admission.crew_source.clone()))
}

fn admitted_reviewer(crew: &Crew, source: String) -> ReviewGateReviewer {
    ReviewGateReviewer {
        crew: crew.name.clone(),
        crew_source: source,
        provider: crew.assignment.provider.clone(),
        model: crew.assignment.model.clone(),
        reasoning_effort: crew.assignment.effort.map(|effort| effort.to_string()),
    }
}

/// The reviewer a held review settles under: the one that reviewed it.
fn held_reviewer(reviewer: &ReviewerIdentity) -> ReviewGateReviewer {
    ReviewGateReviewer {
        crew: reviewer.crew.clone(),
        crew_source: "held_review".to_string(),
        provider: reviewer.provider.clone(),
        model: reviewer.model.clone(),
        reasoning_effort: reviewer.reasoning_effort.clone(),
    }
}

pub(super) fn reviewer_identity(
    runtime: &OrbitRuntime,
    context: &GateContext,
    reviewer: &ReviewGateReviewer,
) -> Result<ReviewerIdentity, OrbitError> {
    let implementer_model = runtime
        .get_job_run_backend(&context.run_id)?
        .and_then(|run| run.crew_model)
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty());
    Ok(ReviewerIdentity {
        crew: reviewer.crew.clone(),
        provider: reviewer.provider.clone(),
        same_model_as_implementer: implementer_model.as_deref() == Some(reviewer.model.as_str()),
        model: reviewer.model.clone(),
        reasoning_effort: reviewer.reasoning_effort.clone(),
        implementer_model,
    })
}

/// Fail before implementation when the lineage's latest review ran out of
/// minutes without a verdict: a resumed run would otherwise implement again
/// only to be refused at admission. A settled review never blocks here — the
/// next candidate is a new review. This does not reserve an attempt, load Git
/// objects or write a manifest.
fn preflight_budget(
    runtime: &OrbitRuntime,
    context: &GateContext,
) -> Result<ReviewGateAdmissionOutput, OrbitError> {
    let lineage = context.lineage_key();
    if let Some(ledger) = runtime
        .review_store()?
        .review_ledger(&context.workspace_id, &lineage)?
        && let Some(latest) = ledger.latest_attempt()
        && !ledger.reviewed(&latest.candidate, &latest.task_meaning_digest)
        && ledger.remaining_at(Utc::now()).seconds == 0
    {
        return Err(OrbitError::CapabilityDenied(format!(
            "review_budget_exhausted: review_minutes_exhausted for lineage '{lineage}'; {}",
            budget_recovery(context, &lineage)
        )));
    }
    Ok(ReviewGateAdmissionOutput::PreflightPassed {
        lineage_key: lineage,
    })
}

/// The operator's way past an exhausted budget. A claim is one attempt that
/// keys its own lineage and is never resumed, so its budget is not reset:
/// the owner requeues the task, and its next claim continues the candidate
/// with a budget of its own.
fn budget_recovery(context: &GateContext, lineage: &str) -> String {
    let task_id = &context.task_ids[0];
    if context.claimed {
        format!(
            "the owner can run orbit task requeue {task_id} --reason '<decision>'; the task's \
             next claim continues this candidate with a review budget of its own"
        )
    } else {
        format!(
            "an operator can run orbit task review-reset {task_id} --lineage '{lineage}' \
             --reason '<decision>' before resuming, or dispatch a fresh delivery run"
        )
    }
}
