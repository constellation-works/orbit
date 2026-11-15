use std::collections::{BTreeMap, HashMap};

use orbit_types::telemetry::AgentExecutionIdentity;

use serde_json::{Map, Value, json};

use super::super::{AgentFacts, Owner, WindowBounds};
use super::board;
use super::board::ModelRow;
use super::compute::Scales;
use super::execution::{ExecutionCounts, execution_json};
use super::measures::{MEASURE_COUNT, MEASURES, REVIEW_OUTCOME, review_supported, weighted_terms};
use crate::application::agents::score::REVIEW_MIN_SUPPORT;

pub(super) fn crews_json(row: &ModelRow) -> Vec<Value> {
    row.crews
        .iter()
        .map(|(crew, tasks)| json!({"crew": crew, "tasks": tasks}))
        .collect()
}

/// One board row. `effort_split` is the row's [`effort_split`].
pub(super) fn row_json(
    effort_split: Value,
    row: &ModelRow,
    board: &board::Board,
    index: usize,
    scales: &Scales,
    execution: &ExecutionCounts,
) -> Value {
    let placed = &board.placed[index];
    let totals = row.totals();
    let review_counts = review_supported(&totals);
    let (terms, weight) = weighted_terms(&totals, &scales.scales, review_counts);
    let mut measures = Map::new();
    for (position, measure) in MEASURES.iter().enumerate() {
        let total = totals[position];
        let (status, reason) = match (measure.unrecorded, terms[position]) {
            (Some(reason), _) => ("unmeasured", Some(reason)),
            (None, Some(_)) => ("measured", None),
            (None, None) if total.units == 0 => {
                ("unmeasured", Some("no units for this model in the window"))
            }
            (None, None) if measure.key == REVIEW_OUTCOME && !review_counts => (
                "unmeasured",
                Some("fewer reviewed deliveries than the minimum support"),
            ),
            (None, None) => ("no_scale", Some("no noise scale for this measure")),
        };
        let measured = measure.unrecorded.is_none() && total.units > 0;
        measures.insert(
            measure.key.to_string(),
            json!({
                "status": status,
                "reason": reason,
                "weight": measure.weight,
                "units": total.units,
                "unit": measure.units,
                "min_support": (measure.key == REVIEW_OUTCOME).then_some(REVIEW_MIN_SUPPORT),
                "actual": measured.then(|| round(total.actual)),
                "expected": measured.then(|| round(total.expected)),
                "residual": total.residual(measure).filter(|_| measured).map(round),
                // Score points before shrinkage: 10 × w × resid ÷ scale ÷ Σw.
                "contribution": terms[position]
                    .filter(|_| weight > 0.0)
                    .map(|term| round(10.0 * term / weight)),
            }),
        );
    }
    let rank_label = placed.band.map(|band| {
        if placed.band_size > 1 {
            format!("{band}=")
        } else {
            band.to_string()
        }
    });
    let units = row.units();
    json!({
        "model": row.model,
        "rank": placed.band,
        "rank_label": rank_label,
        "band_size": (placed.band_size > 0).then_some(placed.band_size),
        "eligible": placed.eligible,
        "single_tier": placed.eligible.then(|| row.single_tier()).flatten(),
        "ineligible_reason": row.ineligibility().or_else(|| {
            (!placed.eligible).then(|| "no measure with units and a scale".to_string())
        }),
        "top_band_ineligible_reason": placed.top_band_reason,
        "units": units,
        "tasks_by_tier": row.tiers(),
        "score": placed.score().map(round),
        "raw_score": placed.z.map(|z| round(100.0 + 10.0 * z)),
        "z": placed.z.map(round4),
        "z_loo": placed.z_loo.map(round4),
        "z_shrunk": placed.z_shrunk.map(round4),
        "weights_measured": weight,
        "interval": placed.interval.map(|(low, high)| json!({"p10": round(low), "p90": round(high)})),
        "cost": {
            "ratio": placed.cost.ratio.map(round4),
            "per_task": placed.cost.per_task.map(round4),
            "expected_per_task": placed.cost.expected_per_task.map(round4),
            "coverage": round4(placed.cost.coverage),
            "cost_unknown": placed.cost.unknown,
            "pricing": "recorded provider or derived list-price cost of the model's runs",
        },
        "measures": measures,
        "landed_tasks": round(row.landed),
        "landed_rate": (units > 0).then(|| round4(row.landed / units as f64)),
        "crews": crews_json(row),
        "effort_split": effort_split,
        "tiers": tiers_json(row, &scales.scales),
        "lines": { "changed": row.lines, "tasks": row.sized_tasks },
        "execution": execution_json(execution),
    })
}

fn tiers_json(row: &ModelRow, scales: &[Option<f64>; MEASURE_COUNT]) -> Vec<Value> {
    board::tier_points(row, scales)
        .into_iter()
        .map(|point| {
            json!({
                "tier": point.tier,
                "tasks": point.tasks,
                "score": point.score.map(round),
                "cost_per_task": point.cost_per_task.map(round4),
                "cost_coverage": round4(point.cost_coverage),
                "cost_unknown": point.cost_unknown,
            })
        })
        .collect()
}

/// Not-scored charges whose anchor falls in the window. These are problems,
/// not tasks, and they are board-level: a Not-scored charge usually names no model.
pub(super) fn not_scored(facts: &AgentFacts, window: WindowBounds) -> Value {
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    let mut count = 0usize;
    for charge in &facts.charges {
        let Owner::NotScored(reason) = charge.owner else {
            continue;
        };
        if !window.admits(charge.anchor) {
            continue;
        }
        count += 1;
        *reasons.entry(reason).or_default() += 1;
    }
    json!({
        "count": count,
        "reasons": reasons,
    })
}

/// The model's completed implementation invocations in the window by
/// reasoning effort. An invocation with no recorded dispatch identity counts
/// under the effort its run's crew label states when the scorecard's effort
/// key resolved that run's attempt from the label (`opus@medium`), so the
/// split never shows it as unknown while the scorecard row names the effort.
/// `labelled` is [`crate::application::agents::roles::crew_label_run_identities`].
pub(super) fn effort_split(
    facts: &AgentFacts,
    window: WindowBounds,
    model: &str,
    labelled: &HashMap<String, AgentExecutionIdentity>,
) -> Value {
    let invocations: Vec<_> = facts
        .invocations
        .iter_invocations()
        .filter(|fact| {
            window.admits(fact.ts)
                && fact.model.as_deref() == Some(model)
                && matches!(
                    fact.activity_id.as_deref(),
                    Some("implement_one" | "agent_implement")
                )
        })
        .map(
            |fact| match (&fact.execution, labelled.get(&fact.job_run_id)) {
                (None, Some(identity)) => {
                    std::borrow::Cow::Owned(orbit_store::contracts::InvocationAccountingFact {
                        execution: Some(identity.clone()),
                        ..fact.clone()
                    })
                }
                _ => std::borrow::Cow::Borrowed(fact),
            },
        )
        .collect();
    json!(orbit_store::contracts::agent_effort_usage(
        invocations.iter().map(std::convert::AsRef::as_ref)
    ))
}

/// Rounded to two places; `+ 0.0` turns a negative zero into zero.
pub(super) fn round(value: f64) -> Value {
    json!((value * 100.0).round() / 100.0 + 0.0)
}

fn round4(value: f64) -> Value {
    json!((value * 10_000.0).round() / 10_000.0 + 0.0)
}
