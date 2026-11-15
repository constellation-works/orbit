//! Review-gate entry point and admission output.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{ReviewGateAdmissionOutput, ReviewGateRefusal, ReviewTiming};
use serde_json::{Value, json};

use super::super::super::REVIEW_AUDIT;
use super::super::super::admission::run_review_admission;
use super::super::context::{GateContext, admitted_run_id, not_applicable};
use super::super::host_gates::HOST_VALIDATION_STEP_INPUT;
use super::holds::SKIP_REVIEW_REASON;
use super::{admission, holds, reviewer};
use crate::OrbitRuntime;

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
    if let ReviewGateAdmissionOutput::NotApplicable(not_applicable) = &output
        && run_never_reviews(&not_applicable.reason)
    {
        refuse_unreviewed_mutation_proposals(runtime, action, input, &not_applicable.reason)?;
    }
    serde_json::to_value(&output).map_err(|error| {
        DispatchError::action_failed(action, format!("serialize review admission: {error}"))
    })
}

/// Whether a gate that does not apply for `reason` leaves the run with no
/// review at all. The others mean another layer of this run reviews.
fn run_never_reviews(reason: &str) -> bool {
    matches!(
        reason,
        "review_admission_missing"
            | "review_before_pr_off"
            | "review_before_landing_off"
            | "review_policy_after_landing"
            | "no_diff_exemption"
            | SKIP_REVIEW_REASON
    )
}

/// [granted-tools §4] A mutation proposal applies only after the review
/// that approves the diff, so a run that reviews nothing cannot deliver a
/// task carrying one: refuse, naming the proposals, rather than land a diff
/// whose proposed change silently never happens.
fn refuse_unreviewed_mutation_proposals(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    reason: &str,
) -> Result<(), DispatchError> {
    if runtime.worker_invocation().is_some() {
        return Ok(());
    }
    let task_ids = input
        .get("completed_task_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str);
    let mut unapplied = Vec::new();
    for task_id in task_ids {
        let proposals = runtime
            .unapplied_mutation_proposals(task_id)
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
        unapplied.extend(
            proposals
                .into_iter()
                .map(|proposal| format!("{task_id}:{}", proposal.artifact)),
        );
    }
    if unapplied.is_empty() {
        return Ok(());
    }
    Err(DispatchError::DeterministicActionRefused {
        action: action.to_string(),
        message: format!(
            "mutation_proposals_unreviewed: this run reviews nothing ({reason}), but unapplied \
             mutation proposals apply only after a review approves them: {}; deliver the task \
             through a run whose review gate applies",
            unapplied.join(", ")
        ),
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
        Some(step_id) => match holds::completion_rebase(runtime, &run_id, step_id, input)
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
        let (code, setting) = if admission.gates_landing() {
            ("review_before_landing", "review.timing = before_landing")
        } else {
            ("review_before_pr", "review.timing = before_pr")
        };
        return Err(refused(format!(
            "{code}_local_route_refused: this run captured {setting} but delivers locally; \
             ship through the PR route or set review.timing = off"
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
        && holds::light_review_skipped(runtime, &context)
            .map_err(|error| DispatchError::action_failed(action, error.to_string()))?
    {
        return Ok(not_applicable(SKIP_REVIEW_REASON, Some(&admission)));
    }
    let mut awaiting_evidence = false;
    let outcome = holds::awaiting_external_evidence(runtime, &context).and_then(|awaiting| {
        awaiting_evidence = awaiting;
        if awaiting {
            Err(OrbitError::CapabilityDenied(
                "review_awaiting_evidence: named external checks have not arrived for the held \
                 candidate"
                    .into(),
            ))
        } else if preflight {
            reviewer::preflight_budget(runtime, &context)
        } else {
            let host_step = input
                .get(HOST_VALIDATION_STEP_INPUT)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|step| !step.is_empty());
            admission::admit(runtime, &context, &admission, rebase.as_ref(), host_step)
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
