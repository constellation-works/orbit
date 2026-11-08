//! The six ranked measures, their weights and units, and how one implemented
//! task contributes to each. Every count and expectation is the implementer
//! scorecard's: the ranking regroups its terms, it never re-attributes.

use serde_json::{Value, json};

use crate::application::agents::roles::LedgerTask;

/// How many measures the ranking has; per-measure arrays use it.
pub(super) const MEASURE_COUNT: usize = 6;

/// One ranked measure.
pub(super) struct Measure {
    pub(super) key: &'static str,
    pub(super) label: &'static str,
    pub(super) pillar: &'static str,
    /// Weight points out of 100.
    pub(super) weight: f64,
    pub(super) units: &'static str,
    /// `+1` when more is better, `-1` for a problem measure, so a positive
    /// residual is always better than expected.
    pub(super) sign: f64,
    /// `Some(reason)` while the measure's events are not recorded: it
    /// reports as unmeasured and leaves every weight denominator.
    pub(super) unrecorded: Option<&'static str>,
    pub(super) definition: &'static str,
}

pub(super) const ESCAPED_REGRESSIONS: &str = "escaped_regressions";
pub(super) const REVIEW_OUTCOME: &str = "review_outcome";
pub(super) const CREW_REWORK: &str = "crew_rework";

pub(super) const MEASURES: [Measure; MEASURE_COUNT] = [
    Measure {
        key: ESCAPED_REGRESSIONS,
        label: "Escaped regressions",
        pillar: "quality",
        weight: 30.0,
        units: "implemented tasks",
        sign: -1.0,
        unrecorded: None,
        definition: "The implementer scorecard's escaped regressions at severity weight, deduplicated against pre-landing review charges, compared with its size-adjusted expectation.",
    },
    Measure {
        key: REVIEW_OUTCOME,
        label: "Review outcome",
        pillar: "quality",
        weight: 20.0,
        units: "reviewed deliveries",
        sign: 1.0,
        unrecorded: None,
        definition: "+1 per first-pass accept, -1 per reject and -0.5 per accept-with-fixes on the row's candidates, summed per reviewed delivery and compared with the same combination of the scorecard's tier expectations.",
    },
    Measure {
        key: "ci_failures",
        label: "Implementer CI failures",
        pillar: "reliability",
        weight: 10.0,
        units: "implemented tasks",
        sign: -1.0,
        unrecorded: Some("required-check failures are not recorded by cause yet"),
        definition: "Required-check failures on the row's candidate heads classed code.",
    },
    Measure {
        key: "final_recoveries",
        label: "Implementer-fault final recoveries",
        pillar: "reliability",
        weight: 10.0,
        units: "implemented tasks",
        sign: -1.0,
        unrecorded: Some("final-recovery decisions record no fault yet"),
        definition: "Final-recovery decisions whose fault is implementer.",
    },
    Measure {
        key: CREW_REWORK,
        label: "Crew-caused rework",
        pillar: "reliability",
        weight: 5.0,
        units: "implemented tasks",
        sign: -1.0,
        unrecorded: None,
        definition: "The implementer scorecard's crew-caused rework, which already leaves out review verdicts; CI failures and final recoveries are not recorded yet, so nothing else is subtracted.",
    },
    Measure {
        key: "tool_call_errors",
        label: "Tool-call errors",
        pillar: "reliability",
        weight: 5.0,
        units: "tool calls",
        sign: -1.0,
        unrecorded: Some("tool audit events are not attributed to a model yet"),
        definition: "Orbit tool calls in the row's implementing activities classed expected or denied.",
    },
];

/// The order each underlying failure is charged in, first match only.
pub(super) const COUNTING_ORDER: [&str; 4] = [
    "reviewer verdict",
    "CI failure on a candidate head",
    "final recovery",
    "crew-caused rework",
];

/// One task's actual and expected count for each measure; `None` where the
/// task adds no unit (an unrecorded measure, or review outcome on a task no
/// review settled).
pub(super) type Observations = [Option<(f64, f64)>; MEASURE_COUNT];

pub(super) fn observe(task: &LedgerTask) -> Observations {
    let term = |key: &str| {
        (
            task.actual.get(key).copied().unwrap_or(0.0),
            task.expected.get(key).copied().unwrap_or(0.0),
        )
    };
    std::array::from_fn(|index| {
        let measure = &MEASURES[index];
        if measure.unrecorded.is_some() {
            return None;
        }
        match measure.key {
            ESCAPED_REGRESSIONS => Some(term("escaped_regressions")),
            CREW_REWORK => Some(term("crew_rework")),
            REVIEW_OUTCOME if task.reviewed => {
                let (accepts, expected_accepts) = term("first_pass_accepts");
                let (rejects, expected_rejects) = term("review_rejects");
                let (repairs, expected_repairs) = term("reviewer_repairs");
                Some((
                    accepts - rejects - 0.5 * repairs,
                    expected_accepts - expected_rejects - 0.5 * expected_repairs,
                ))
            }
            _ => None,
        }
    })
}

/// A measure's totals over a set of tasks.
#[derive(Clone, Copy, Default)]
pub(super) struct Totals {
    pub(super) actual: f64,
    pub(super) expected: f64,
    pub(super) units: usize,
}

impl Totals {
    /// `sign × (actual − expected) ÷ units`; `None` without units.
    pub(super) fn residual(self, measure: &Measure) -> Option<f64> {
        (self.units > 0).then(|| measure.sign * (self.actual - self.expected) / self.units as f64)
    }
}

pub(super) fn totals<'a>(
    observations: impl IntoIterator<Item = &'a Observations>,
) -> [Totals; MEASURE_COUNT] {
    let mut totals = [Totals::default(); MEASURE_COUNT];
    for observed in observations {
        for (total, observation) in totals.iter_mut().zip(observed) {
            if let Some((actual, expected)) = observation {
                total.actual += actual;
                total.expected += expected;
                total.units += 1;
            }
        }
    }
    totals
}

/// Each measure's weighted noise-unit term, `w × resid ÷ scale`, for the
/// measures that have units and a scale, and their weight sum.
pub(super) fn weighted_terms(
    totals: &[Totals; MEASURE_COUNT],
    scales: &[Option<f64>; MEASURE_COUNT],
) -> ([Option<f64>; MEASURE_COUNT], f64) {
    let mut weight = 0.0;
    let terms = std::array::from_fn(|index| {
        let measure = &MEASURES[index];
        let scale = scales[index].filter(|scale| *scale > 0.0)?;
        let residual = totals[index].residual(measure)?;
        weight += measure.weight;
        Some(measure.weight * residual / scale)
    });
    (terms, weight)
}

/// The composite `z`: weighted noise-unit residuals over the weights of
/// the measures that count for this row. `None` when none does.
pub(super) fn composite(
    totals: &[Totals; MEASURE_COUNT],
    scales: &[Option<f64>; MEASURE_COUNT],
) -> Option<f64> {
    let (terms, weight) = weighted_terms(totals, scales);
    (weight > 0.0).then(|| terms.iter().flatten().sum::<f64>() / weight)
}

/// The measure list every response carries, with the scales in force.
pub(super) fn measures_json(
    scales: &[Option<f64>; MEASURE_COUNT],
    floors: &[Option<f64>; MEASURE_COUNT],
) -> Vec<Value> {
    MEASURES
        .iter()
        .enumerate()
        .map(|(index, measure)| {
            let status = match (measure.unrecorded, scales[index]) {
                (Some(_), _) => "unmeasured",
                (None, Some(scale)) if scale > 0.0 => "measured",
                (None, _) => "no_scale",
            };
            json!({
                "key": measure.key,
                "label": measure.label,
                "pillar": measure.pillar,
                "weight": measure.weight,
                "units": measure.units,
                "sign": measure.sign,
                "definition": measure.definition,
                "status": status,
                "reason": measure.unrecorded.or(match status {
                    "no_scale" => Some("no variation to scale by: the measure never moved in the calibration sample"),
                    _ => None,
                }),
                "scale": scales[index],
                "floor": floors[index],
            })
        })
        .collect()
}
