//! Selective before-landing evaluation at the landing gate: measure the
//! complete pinned base/head diff, apply the captured policy's OR
//! predicates, and persist the host-authored selection receipt. The
//! candidate's own selectors and configuration never feed the decision.

use orbit_common::OrbitError;
use orbit_engine::review_gate::{candidate_identity, changed_path_pairs, diff_lines_between};
use orbit_types::workflow::{
    REVIEW_SELECTION_ARTIFACT, SelectiveBeforeLandingMode, SelectiveBeforeLandingPolicy,
    SelectiveEvaluationInputs, SelectiveReviewReceipt, SelectiveReviewResult, SelectiveTaskRisk,
    evaluate_selective_review,
};

use super::context::GateContext;
use super::judgement::write_artifact;
use crate::OrbitRuntime;

/// The decision the landing gate acts on.
pub(super) enum SelectiveGateDecision {
    /// Enforcement selected this candidate: admit the existing reviewer
    /// with this receipt attached. Skip tags cannot bypass it.
    Required(SelectiveReviewReceipt),
    /// No reviewer runs: enforcement did not select, or the mode only
    /// observes. The receipt records why (and, in observe mode, what
    /// enforcement would have required).
    NotRequired(SelectiveReviewReceipt),
}

/// Evaluate the captured selective policy against the pinned candidate
/// and persist the receipt. An identical base/head/policy/model retry
/// reuses the stored receipt instead of re-measuring; a lineage an
/// earlier receipt selected can never downgrade.
pub(super) fn selective_gate_decision(
    runtime: &OrbitRuntime,
    context: &GateContext,
    policy: &SelectiveBeforeLandingPolicy,
) -> Result<SelectiveGateDecision, OrbitError> {
    let receipt = evaluate_and_persist(runtime, context, policy)?;
    if receipt.result == SelectiveReviewResult::Unavailable
        && policy.mode == SelectiveBeforeLandingMode::Enforce
    {
        return Err(OrbitError::CapabilityDenied(format!(
            "selection_unavailable: selective before-landing enforcement cannot decide this \
             candidate: {}; enforced completion is held regardless of unknown_risk",
            receipt.gaps.join("; ")
        )));
    }
    Ok(if receipt.requires_review() {
        SelectiveGateDecision::Required(receipt)
    } else {
        SelectiveGateDecision::NotRequired(receipt)
    })
}

fn evaluate_and_persist(
    runtime: &OrbitRuntime,
    context: &GateContext,
    policy: &SelectiveBeforeLandingPolicy,
) -> Result<SelectiveReviewReceipt, OrbitError> {
    let candidate = candidate_identity(&context.workspace_path, &context.base_sha()?)?;
    let prior = prior_receipt(runtime, context)?;
    if let Some(prior) = &prior
        && prior.covers(
            &policy.digest,
            &candidate.base.commit,
            &candidate.head.commit,
            &context.task_ids,
        )
    {
        return Ok(prior.clone());
    }
    let previously_selected = prior.as_ref().is_some_and(|prior| {
        prior.task_ids == context.task_ids
            && prior.mode == SelectiveBeforeLandingMode::Enforce
            && prior.result == SelectiveReviewResult::Required
    });

    let mut inputs = SelectiveEvaluationInputs {
        previously_selected,
        ..SelectiveEvaluationInputs::default()
    };
    match changed_path_pairs(
        &context.workspace_path,
        &candidate.base.commit,
        &candidate.head.commit,
    ) {
        Ok(pairs) => inputs.changed_paths = pairs,
        Err(error) => {
            inputs.path_diff_gap = Some(format!("candidate path diff unreadable: {error}"));
        }
    }
    let lines = match diff_lines_between(
        &context.workspace_path,
        &candidate.base.commit,
        &candidate.head.commit,
    ) {
        Ok(lines) => Some(lines),
        Err(error) => {
            if inputs.path_diff_gap.is_none() {
                inputs.path_diff_gap = Some(format!("candidate diff size unreadable: {error}"));
            }
            None
        }
    };
    inputs.diff_lines = lines;

    if policy.risk_threshold.is_some() && inputs.path_diff_gap.is_none() {
        let frozen = prior
            .as_ref()
            .and_then(|prior| prior.risk_model_identity.as_deref())
            .and_then(frozen_fit);
        let (probability, identity, model_gap) = match (lines, frozen) {
            (Some(lines), Some((intercept, slope, identity))) => (
                Some(fit_probability(intercept, slope, lines)),
                Some(identity),
                None,
            ),
            (Some(lines), None) => match runtime.task_riskiness_model() {
                Ok(model) => (
                    model.estimate_diff_lines(lines),
                    Some(model.diff_model_identity()),
                    None,
                ),
                Err(error) => (None, None, Some(format!("risk model unavailable: {error}"))),
            },
            _ => (
                None,
                None,
                Some("candidate diff size unavailable".to_string()),
            ),
        };
        inputs.risk_model_identity = identity;
        if context.task_ids.len() == 1 {
            let task_id = context.task_ids[0].clone();
            inputs.task_risks = vec![match probability {
                Some(probability) => SelectiveTaskRisk::estimated(task_id, lines, probability),
                None => SelectiveTaskRisk::unknown(
                    task_id,
                    lines,
                    model_gap.unwrap_or_else(|| {
                        "insufficient diff-lines history for a usable fit".to_string()
                    }),
                ),
            }];
        } else {
            // The combined diff of several tasks is not a calibrated
            // per-task probability; shared attribution is unknown, never
            // an aggregate invented from the whole.
            inputs.task_risks = context
                .task_ids
                .iter()
                .map(|task_id| {
                    SelectiveTaskRisk::unknown(
                        task_id.clone(),
                        None,
                        "shared/partial attribution across multiple tasks".to_string(),
                    )
                })
                .collect();
        }
    }

    let receipt = evaluate_selective_review(
        policy,
        &context.workspace_id,
        &context.run_id,
        &context.task_ids,
        (&candidate.base.commit, &candidate.base.tree),
        (&candidate.head.commit, &candidate.head.tree),
        inputs,
    );
    let content = serde_json::to_vec_pretty(&receipt).map_err(|error| {
        OrbitError::Execution(format!("serialize selective review receipt: {error}"))
    })?;
    write_artifact(
        runtime,
        &context.task_ids[0],
        &context.run_id,
        REVIEW_SELECTION_ARTIFACT,
        &content,
    )?;
    tracing::info!(
        run_id = %context.run_id,
        result = receipt.result.as_str(),
        %receipt.policy_digest,
        "selective before-landing selection: {}",
        receipt.summary()
    );
    Ok(receipt)
}

/// The lineage's stored receipt, when one was persisted for the first
/// task. A malformed receipt is ignored as evidence (selection is
/// re-evaluated), never trusted.
fn prior_receipt(
    runtime: &OrbitRuntime,
    context: &GateContext,
) -> Result<Option<SelectiveReviewReceipt>, OrbitError> {
    let Some(artifact) =
        runtime.get_task_artifact(&context.task_ids[0], REVIEW_SELECTION_ARTIFACT)?
    else {
        return Ok(None);
    };
    Ok(serde_json::from_slice(&artifact.content).ok())
}

/// The `(intercept, slope, identity)` a prior receipt froze, so revised
/// heads in the lineage keep the model the delivery started with instead
/// of refitting against unrelated later history.
fn frozen_fit(identity: &str) -> Option<(f64, f64, String)> {
    let coefficient = |name: &str| {
        identity
            .split_whitespace()
            .find_map(|field| field.strip_prefix(name))
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite())
    };
    Some((
        coefficient("intercept=")?,
        coefficient("slope=")?,
        identity.to_string(),
    ))
}

/// The task-riskiness fit's probability for a measured size:
/// `1 - exp(-exp(a + b·ln(1 + size)))`, as the estimator computes it.
fn fit_probability(intercept: f64, slope: f64, lines: u64) -> f64 {
    let hazard = (intercept + slope * (lines as f64).ln_1p()).exp();
    -(-hazard).exp_m1()
}
