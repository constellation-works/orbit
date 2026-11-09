use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use super::super::{AgentFacts, Owner, WindowBounds};
use super::board;
use super::board::ModelRow;
use super::compute::Scales;
use super::execution::{ExecutionCounts, execution_json};
use super::measures::{MEASURE_COUNT, MEASURES, weighted_terms};

pub(super) fn crews_json(row: &ModelRow) -> Vec<Value> {
    row.crews
        .iter()
        .map(|(crew, tasks)| json!({"crew": crew, "tasks": tasks}))
        .collect()
}

pub(super) fn row_json(
    facts: &AgentFacts,
    window: WindowBounds,
    row: &ModelRow,
    board: &board::Board,
    index: usize,
    scales: &Scales,
    execution: &ExecutionCounts,
) -> Value {
    let placed = &board.placed[index];
    let totals = row.totals();
    let (terms, weight) = weighted_terms(&totals, &scales.scales);
    let mut measures = Map::new();
    for (position, measure) in MEASURES.iter().enumerate() {
        let total = totals[position];
        let (status, reason) = match (measure.unrecorded, terms[position]) {
            (Some(reason), _) => ("unmeasured", Some(reason)),
            (None, Some(_)) => ("measured", None),
            (None, None) if total.units == 0 => {
                ("unmeasured", Some("no units for this model in the window"))
            }
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
        "ineligible_reason": row.ineligibility().or_else(|| {
            (!placed.eligible).then(|| "no measure with units and a scale".to_string())
        }),
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
        "effort_split": effort_split(facts, window, &row.model),
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
/// reasoning effort.
fn effort_split(facts: &AgentFacts, window: WindowBounds, model: &str) -> Value {
    json!(orbit_store::contracts::agent_effort_usage(
        facts.invocations.iter().filter(|fact| {
            window.admits(fact.ts)
                && fact.model.as_deref() == Some(model)
                && matches!(
                    fact.activity_id.as_deref(),
                    Some("implement_one" | "agent_implement")
                )
        })
    ))
}

/// Rounded to two places; `+ 0.0` turns a negative zero into zero.
pub(super) fn round(value: f64) -> Value {
    json!((value * 100.0).round() / 100.0 + 0.0)
}

fn round4(value: f64) -> Value {
    json!((value * 10_000.0).round() / 10_000.0 + 0.0)
}
