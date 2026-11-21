use orbit_types::task::DecisionRequestState;
use serde_json::{Value, json};

use super::measure::MeasureInputs;
use super::window;

pub(super) enum MetricsReportKind {
    Mandate,
    Baseline,
}

fn escalation_json(
    escalations: &[orbit_types::task::DecisionRequest],
    active_days: f64,
    human_days: f64,
) -> Value {
    let mut answered = Vec::new();
    let mut open = 0usize;
    let mut expired_unanswered = 0usize;
    let mut superseded = 0usize;
    for request in escalations {
        match request.state {
            DecisionRequestState::Answered => answered.push(request),
            DecisionRequestState::Open => open += 1,
            DecisionRequestState::Expired => expired_unanswered += 1,
            DecisionRequestState::Superseded => superseded += 1,
        }
    }
    let latencies: Vec<f64> = answered
        .iter()
        .filter_map(|request| {
            request.resolution.as_ref().map(|resolution| {
                (resolution.answered_at - request.raised_at)
                    .num_seconds()
                    .max(0) as f64
            })
        })
        .collect();
    let recommended_answered = answered
        .iter()
        .filter(|request| {
            request.recommended.is_some()
                && request.resolution.as_ref().map(|r| &r.answer_id) == request.recommended.as_ref()
        })
        .count();
    let with_recommendation = answered
        .iter()
        .filter(|request| request.recommended.is_some())
        .count();
    json!({
        "count": escalations.len(),
        "total": escalations.len(),
        "per_mandate_day": escalations.len() as f64 / active_days,
        "per_human_day": escalations.len() as f64 / human_days,
        "answered": answered.len(),
        "open": open,
        "expired_unanswered": expired_unanswered,
        "superseded": superseded,
        "median_answer_latency_seconds": window::median_seconds(&latencies),
        "answer_latency_seconds": latencies,
        "recommended_share": if with_recommendation > 0 {
            json!(recommended_answered as f64 / with_recommendation as f64)
        } else {
            Value::Null
        },
        "share_answered_with_recommended": if with_recommendation > 0 {
            json!(recommended_answered as f64 / with_recommendation as f64)
        } else {
            Value::Null
        },
        "requests": escalations.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
    })
}

fn reversal_json(inputs: &MeasureInputs) -> Value {
    json!({
        "total": inputs.reversal_total,
        "decisions_and_actions": inputs.reversal_total,
        "reversed": inputs.reversed,
        "reversal_count": inputs.reversed,
        "rate": if inputs.reversal_total > 0 {
            json!(inputs.reversed as f64 / inputs.reversal_total as f64)
        } else {
            Value::Null
        },
        "reversal_rate": if inputs.reversal_total > 0 {
            json!(inputs.reversed as f64 / inputs.reversal_total as f64)
        } else {
            Value::Null
        },
        "reversal_window_days": window::REVERSAL_WINDOW_DAYS,
        "reversed_subjects": inputs.reversed_subjects,
    })
}

fn recovery_json(inputs: &MeasureInputs) -> Value {
    json!({
        "crashed_cycles": inputs.crashed,
        "cycles_without_checkpoint": inputs.crashed,
        "incomplete_checkpoints": inputs.crashed,
        "time_to_next_good_checkpoint_seconds": {
            "median": window::median_seconds(&inputs.recovery_seconds),
            "max": inputs.recovery_seconds.iter().copied().fold(None::<f64>, |acc, v| {
                Some(acc.map_or(v, |a: f64| a.max(v)))
            }),
            "samples": inputs.recovery_seconds,
        },
        "median_time_to_next_good_checkpoint_seconds": window::median_seconds(&inputs.recovery_seconds),
        "recovery_slot_minutes": inputs.recovery_slot_minutes,
        "repeated_actions": inputs.repeated_actions,
        "human_interventions_needed": inputs.recovery_interventions,
    })
}

pub(super) fn mandate_json(
    task_id: &str,
    active_days: f64,
    human_days: f64,
    inputs: &MeasureInputs,
    kind: MetricsReportKind,
) -> Value {
    let interventions = json!({
        "count": inputs.interventions.len(),
        "total": inputs.interventions.len(),
        "per_mandate": inputs.interventions.len(),
        "per_mandate_day": inputs.interventions.len() as f64 / active_days,
        "active_days": active_days,
        "items": inputs.interventions,
    });
    let escalations = escalation_json(&inputs.escalations, active_days, human_days);
    let references = json!({
        "foreign": inputs.foreign_references,
        "unreachable": inputs.unreachable_references,
    });
    if matches!(kind, MetricsReportKind::Baseline) && !inputs.has_checkpoints {
        // An interactive loop leaves no checkpoint decisions and no cycle
        // record: reversals and crash recovery cannot be derived.
        return json!({
            "task_id": task_id,
            "baseline": true,
            "active_days": active_days,
            "interventions": interventions,
            "manual_interventions": interventions,
            "escalations": escalations,
            "escalation_load": escalations,
            "reversals": "not_derivable",
            "reversal_rate": "not_derivable",
            "recovery": "not_derivable",
            "recovery_cost": "not_derivable",
            "references": references,
        });
    }
    let reversals = reversal_json(inputs);
    let recovery = recovery_json(inputs);
    json!({
        "task_id": task_id,
        "active_days": active_days,
        "interventions": interventions.clone(),
        "manual_interventions": interventions,
        "escalations": escalations.clone(),
        "escalation_load": escalations,
        "reversals": reversals.clone(),
        "reversal_rate": reversals,
        "recovery": recovery.clone(),
        "recovery_cost": recovery,
        "references": references,
    })
}

pub(super) fn totals_json(window_days: f64, mandate_days: f64, totals: &MeasureInputs) -> Value {
    // Workspace totals divide per-mandate-day rates by summed mandate active
    // days (mandate-days), not by the calendar window. Per-human-day rates
    // divide by the calendar window days (`human_days`).
    let window_days = window_days.max(1.0);
    let mandate_days = mandate_days.max(1.0);
    let mut aggregate = mandate_json(
        "totals",
        mandate_days,
        window_days,
        totals,
        MetricsReportKind::Mandate,
    );
    aggregate["task_id"] = Value::Null;
    aggregate
}
