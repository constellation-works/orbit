//! The board (§4.3–§4.5): pool crews by model, take each model's composite,
//! shrink it toward the other eligible models, bootstrap it within tiers
//! for the interval and the paired probabilities, band it and order each
//! band by score, then cost and model name for exact score ties.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use super::measures::{
    MEASURE_COUNT, MEASURES, Totals, composite, observe, review_supported, totals, weighted_terms,
};
use super::sample::{BOOTSTRAP_SEED, DRAWS, Record, by_tier, draw};
use crate::application::agents::roles::LedgerRow;
use crate::application::agents::{CREW_UNKNOWN, MODEL_UNKNOWN};
use crate::application::statistics::{SplitMix64, percentile};

/// Units of the other models' evidence a score is blended with.
pub(super) const SHRINKAGE_K: f64 = 30.0;
/// Implemented tasks a model needs before it is eligible.
pub(super) const MIN_TASKS: usize = 30;
/// Assessed tiers an eligible model spans, each with `MIN_TIER_TASKS`.
pub(super) const MIN_TIERS: usize = 1;
pub(super) const MIN_TIER_TASKS: usize = 5;
/// Eligible models a qualified board needs.
pub(super) const MIN_ELIGIBLE_MODELS: usize = 3;
/// A model starts a new band when the one above beats it at least this
/// often.
pub(super) const BAND_PROBABILITY: f64 = 0.95;
/// Cost coverage below which cost is unknown when breaking exact score ties.
pub(super) const MIN_COST_COVERAGE: f64 = 0.8;
pub(super) const INTERVAL: (f64, f64) = (10.0, 90.0);

/// One model's pooled tasks, across every crew and effort level.
pub(super) struct ModelRow {
    pub(super) model: String,
    pub(super) crews: BTreeMap<String, usize>,
    pub(super) records: Vec<Record>,
    /// Tier and recorded cost per task.
    pub(super) costs: Vec<(&'static str, Option<f64>)>,
    pub(super) landed: f64,
    /// Landed lines summed across tasks that recorded a diff size. A task
    /// two crews of one model both worked is summed once per crew, as the
    /// scorecard pools that task.
    pub(super) lines: u64,
    /// How many of those tasks recorded a diff size.
    pub(super) sized_tasks: usize,
}

impl ModelRow {
    pub(super) fn units(&self) -> usize {
        self.records.len()
    }

    pub(super) fn tiers(&self) -> BTreeMap<&'static str, usize> {
        let mut tiers = BTreeMap::new();
        for record in &self.records {
            *tiers.entry(record.tier).or_default() += 1;
        }
        tiers
    }

    /// Assessed tiers holding at least `MIN_TIER_TASKS` tasks.
    pub(super) fn assessed_tiers(&self) -> Vec<&'static str> {
        self.tiers()
            .into_iter()
            .filter(|(tier, count)| *tier != "unassessed" && *count >= MIN_TIER_TASKS)
            .map(|(tier, _)| tier)
            .collect()
    }

    /// The sole assessed tier when the model meets task and tier thresholds
    /// and spans exactly one. The caller must also check board eligibility.
    pub(super) fn single_tier(&self) -> Option<&'static str> {
        if self.ineligibility().is_some() {
            return None;
        }
        let tiers = self.assessed_tiers();
        (tiers.len() == 1).then(|| tiers[0])
    }

    /// Why the model is not eligible, or `None` when it is.
    pub(super) fn ineligibility(&self) -> Option<String> {
        if self.model == MODEL_UNKNOWN {
            return Some("unattributed: the implementation runs recorded no model".into());
        }
        if self.units() < MIN_TASKS {
            return Some(format!(
                "{} implemented tasks; {MIN_TASKS} needed",
                self.units()
            ));
        }
        let tiers = self.assessed_tiers().len();
        (tiers < MIN_TIERS).then(|| {
            format!(
                "{tiers} assessed tiers with at least {MIN_TIER_TASKS} tasks; {MIN_TIERS} needed"
            )
        })
    }

    pub(super) fn totals(&self) -> [Totals; MEASURE_COUNT] {
        totals(self.records.iter().map(|record| &record.observed))
    }
}

/// Pool the scorecard's crew-and-model rows by model. A task two crews of
/// one model both worked counts once per crew, as on the scorecard.
pub(super) fn pool(ledger: Vec<LedgerRow>) -> Vec<ModelRow> {
    let mut models: BTreeMap<String, ModelRow> = BTreeMap::new();
    for row in ledger {
        let model = models.entry(row.model.clone()).or_insert_with(|| ModelRow {
            model: row.model.clone(),
            crews: BTreeMap::new(),
            records: Vec::new(),
            costs: Vec::new(),
            landed: 0.0,
            lines: 0,
            sized_tasks: 0,
        });
        for task in row.tasks {
            // Credit each crew only with the tasks it actually worked: a
            // pooled row's tasks belong to different crews' subsets.
            if task.crews.is_empty() {
                *model.crews.entry(CREW_UNKNOWN.to_string()).or_default() += 1;
            }
            for crew in &task.crews {
                *model.crews.entry(crew.clone()).or_default() += 1;
            }
            model.landed += task.actual.get("weighted_landed").copied().unwrap_or(0.0);
            if let Some(lines) = task.lines {
                model.lines = model.lines.saturating_add(lines);
                model.sized_tasks += 1;
            }
            model.costs.push((task.tier, task.cost));
            model.records.push(Record {
                tier: task.tier,
                start: task.start,
                observed: observe(&task),
            });
        }
    }
    models.into_values().collect()
}

/// One model's place on the board.
pub(super) struct Placed {
    pub(super) z: Option<f64>,
    pub(super) z_loo: Option<f64>,
    pub(super) z_shrunk: Option<f64>,
    pub(super) eligible: bool,
    /// Why the model cannot hold the top band, when it lacks a measure the
    /// board's best-covered eligible models were scored on.
    pub(super) top_band_reason: Option<String>,
    pub(super) interval: Option<(f64, f64)>,
    /// Shared rank of the model's band on a qualified board.
    pub(super) band: Option<usize>,
    pub(super) band_size: usize,
    pub(super) cost: Cost,
}

impl Placed {
    pub(super) fn score(&self) -> Option<f64> {
        self.z_shrunk.map(|z| 100.0 + 10.0 * z)
    }
}

/// A model's list-price cost per task against the fleet's on its tier mix.
#[derive(Default)]
pub(super) struct Cost {
    pub(super) per_task: Option<f64>,
    pub(super) expected_per_task: Option<f64>,
    pub(super) ratio: Option<f64>,
    pub(super) coverage: f64,
    pub(super) unknown: bool,
}

/// The computed board: one placement per row, in row order, the display
/// order, and the paired probabilities among eligible models.
pub(super) struct Board {
    pub(super) placed: Vec<Placed>,
    pub(super) order: Vec<usize>,
    pub(super) qualified: bool,
    /// `P(a > b)` for eligible `a != b`, keyed by row index.
    pub(super) paired: BTreeMap<(usize, usize), f64>,
}

/// Units-weighted mean `z` of every eligible row but `skip`; `None` when
/// there is no other.
fn leave_one_out(z: &[Option<f64>], units: &[f64], eligible: &[bool], skip: usize) -> Option<f64> {
    let (sum, weight) = (0..z.len())
        .filter(|&other| other != skip && eligible[other])
        .filter_map(|other| z[other].map(|z| (z * units[other], units[other])))
        .fold((0.0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1));
    (weight > 0.0).then(|| sum / weight)
}

fn shrink(z: f64, units: f64, z_loo: Option<f64>) -> f64 {
    (units * z + SHRINKAGE_K * z_loo.unwrap_or(0.0)) / (units + SHRINKAGE_K)
}

pub(super) fn compute(rows: &[ModelRow], scales: &[Option<f64>; MEASURE_COUNT]) -> Board {
    let units: Vec<f64> = rows.iter().map(|row| row.units() as f64).collect();
    // Per row: which measures scored it, and their weight.
    let (scored, measured): (Vec<[bool; MEASURE_COUNT]>, Vec<f64>) = rows
        .iter()
        .map(|row| {
            let totals = row.totals();
            let (terms, weight) = weighted_terms(&totals, scales, review_supported(&totals));
            (terms.map(|term| term.is_some()), weight)
        })
        .unzip();
    let z: Vec<Option<f64>> = rows
        .iter()
        .map(|row| {
            let totals = row.totals();
            composite(&totals, scales, review_supported(&totals))
        })
        .collect();
    let eligible: Vec<bool> = rows
        .iter()
        .zip(&z)
        .map(|(row, z)| row.ineligibility().is_none() && z.is_some())
        .collect();
    let eligible_rows: Vec<usize> = (0..rows.len()).filter(|&index| eligible[index]).collect();
    let qualified = eligible_rows.len() >= MIN_ELIGIBLE_MODELS;

    // Bootstrap: resample each eligible model's tasks within its tiers,
    // expectations and scales fixed, and shrink every draw the same way.
    let strata: Vec<_> = eligible_rows
        .iter()
        .map(|&index| {
            let strata = by_tier(&rows[index].records);
            let per_tier: Vec<_> = strata
                .iter()
                .map(|(&tier, records)| (tier, records.len()))
                .collect();
            (strata, per_tier)
        })
        .collect();
    // Whether a model's review outcome counts is fixed by its own reviewed
    // deliveries, not redrawn with each resample.
    let review_counts: Vec<bool> = rows
        .iter()
        .map(|row| review_supported(&row.totals()))
        .collect();
    let mut rng = SplitMix64::seeded(BOOTSTRAP_SEED);
    let mut draws: Vec<Vec<f64>> = vec![Vec::with_capacity(DRAWS); eligible_rows.len()];
    for _ in 0..DRAWS {
        let mut drawn = z.clone();
        for (position, &index) in eligible_rows.iter().enumerate() {
            let (strata, per_tier) = &strata[position];
            let sample = draw(&mut rng, strata, per_tier);
            drawn[index] = composite(&totals(sample), scales, review_counts[index]).or(z[index]);
        }
        for (position, &index) in eligible_rows.iter().enumerate() {
            let z_loo = leave_one_out(&drawn, &units, &eligible, index);
            let shrunk = shrink(drawn[index].unwrap_or(0.0), units[index], z_loo);
            draws[position].push(100.0 + 10.0 * shrunk);
        }
    }
    let mut paired = BTreeMap::new();
    for (a_position, &a) in eligible_rows.iter().enumerate() {
        for (b_position, &b) in eligible_rows.iter().enumerate() {
            if a == b {
                continue;
            }
            let wins: f64 = draws[a_position]
                .iter()
                .zip(&draws[b_position])
                .map(|(a, b)| match a.total_cmp(b) {
                    Ordering::Greater => 1.0,
                    Ordering::Equal => 0.5,
                    Ordering::Less => 0.0,
                })
                .sum();
            paired.insert((a, b), wins / DRAWS as f64);
        }
    }

    let costs = costs(rows);
    let mut placed: Vec<Placed> = rows
        .iter()
        .enumerate()
        .zip(costs)
        .map(|((index, _), cost)| {
            let z_loo = leave_one_out(&z, &units, &eligible, index);
            let interval =
                eligible_rows
                    .iter()
                    .position(|&row| row == index)
                    .and_then(|position| {
                        let mut scores = draws[position].clone();
                        Some((
                            percentile(&mut scores, INTERVAL.0)?,
                            percentile(&mut scores, INTERVAL.1)?,
                        ))
                    });
            Placed {
                z: z[index],
                z_loo,
                z_shrunk: z[index].map(|z| shrink(z, units[index], z_loo)),
                eligible: eligible[index],
                top_band_reason: None,
                interval,
                band: None,
                band_size: 0,
                cost,
            }
        })
        .collect();

    let scores: Vec<f64> = placed
        .iter()
        .map(|placed| placed.score().unwrap_or(f64::NEG_INFINITY))
        .collect();
    // Cost breaks exact score ties only. A model without enough cost
    // coverage follows priced models on an exact tie.
    let cost_keys: Vec<(bool, Option<f64>)> = placed
        .iter()
        .map(|placed| {
            let cost = &placed.cost;
            (cost.unknown, if cost.unknown { None } else { cost.ratio })
        })
        .collect();
    let by_score = |a: &usize, b: &usize| {
        scores[*b]
            .total_cmp(&scores[*a])
            .then_with(|| rows[*a].model.cmp(&rows[*b].model))
    };
    let by_exact_score_tie = |a: &usize, b: &usize| {
        let (a_unknown, a_ratio) = cost_keys[*a];
        let (b_unknown, b_ratio) = cost_keys[*b];
        a_unknown
            .cmp(&b_unknown)
            .then_with(|| match (a_ratio, b_ratio) {
                (Some(a), Some(b)) => a.total_cmp(&b),
                _ => Ordering::Equal,
            })
            .then_with(|| rows[*a].model.cmp(&rows[*b].model))
    };
    let mut ranked = eligible_rows.clone();
    ranked.sort_by(by_score);
    let mut rest: Vec<usize> = (0..rows.len()).filter(|index| !eligible[*index]).collect();
    rest.sort_by(by_score);
    let mut order = Vec::with_capacity(rows.len());
    if qualified {
        let best_weight = eligible_rows
            .iter()
            .map(|&index| measured[index])
            .fold(0.0, f64::max);
        let full_weight = |index: usize| measured[index] >= best_weight;
        let reference = eligible_rows
            .iter()
            .copied()
            .find(|&index| full_weight(index));
        for &index in &ranked {
            if full_weight(index) {
                continue;
            }
            // Measures the best-covered model was scored on and this one was not.
            let lacking: Vec<&str> = (0..MEASURE_COUNT)
                .filter(|&m| reference.is_some_and(|r| scored[r][m]) && !scored[index][m])
                .map(|m| MEASURES[m].label)
                .collect();
            placed[index].top_band_reason = Some(format!(
                "measured weight {} of {best_weight} points, without {}: the top band takes only models scored on the best-covered set",
                measured[index],
                lacking.join(", ")
            ));
        }
        // Each band is the leading run of the models still unbanded, joined
        // while each joins the one above it. The top band draws only from
        // models at the board's full measured weight: a model scored on fewer
        // measures (review outcome unmeasured) is not comparable with them and
        // can still rank in a lower band.
        let joins = |above: usize, below: usize| {
            paired
                .get(&(above, below))
                .is_some_and(|p| *p < BAND_PROBABILITY)
        };
        let mut bands: Vec<Vec<usize>> = Vec::new();
        let mut remaining = ranked.clone();
        while !remaining.is_empty() {
            let pool: Vec<usize> = if bands.is_empty() {
                remaining
                    .iter()
                    .copied()
                    .filter(|&index| full_weight(index))
                    .collect()
            } else {
                remaining.clone()
            };
            let mut band = vec![pool[0]];
            for pair in pool.windows(2) {
                if !joins(pair[0], pair[1]) {
                    break;
                }
                band.push(pair[1]);
            }
            remaining.retain(|index| !band.contains(index));
            bands.push(band);
        }
        for (band_index, mut band) in bands.into_iter().enumerate() {
            // Dense band ranks: each band takes the next rank, however many
            // models share the band above it.
            let rank = band_index + 1;
            band.sort_by(|a, b| {
                scores[*b]
                    .total_cmp(&scores[*a])
                    .then_with(|| by_exact_score_tie(a, b))
            });
            for &index in &band {
                placed[index].band = Some(rank);
                placed[index].band_size = band.len();
            }
            order.extend(band);
        }
    } else {
        order.extend(ranked);
    }
    order.extend(rest);
    Board {
        placed,
        order,
        qualified,
        paired,
    }
}

/// Each row's cost per task against the fleet's per-tier cost per task on
/// the row's own priced tier mix.
fn costs(rows: &[ModelRow]) -> Vec<Cost> {
    let mut fleet: BTreeMap<&str, (f64, usize)> = BTreeMap::new();
    for (tier, cost) in rows.iter().flat_map(|row| &row.costs) {
        if let Some(cost) = cost {
            let entry = fleet.entry(tier).or_default();
            entry.0 += cost;
            entry.1 += 1;
        }
    }
    rows.iter()
        .map(|row| {
            let priced: Vec<_> = row
                .costs
                .iter()
                .filter_map(|(tier, cost)| cost.map(|cost| (*tier, cost)))
                .collect();
            let coverage = if row.costs.is_empty() {
                0.0
            } else {
                priced.len() as f64 / row.costs.len() as f64
            };
            let per_task = (!priced.is_empty())
                .then(|| priced.iter().map(|(_, cost)| cost).sum::<f64>() / priced.len() as f64);
            let expected_per_task = (!priced.is_empty()).then(|| {
                priced
                    .iter()
                    .map(|(tier, _)| {
                        let (total, count) = fleet[tier];
                        total / count as f64
                    })
                    .sum::<f64>()
                    / priced.len() as f64
            });
            let ratio = per_task
                .zip(expected_per_task)
                .filter(|(_, expected)| *expected > 0.0)
                .map(|(cost, expected)| cost / expected);
            Cost {
                per_task,
                expected_per_task,
                ratio,
                coverage,
                unknown: coverage < MIN_COST_COVERAGE || ratio.is_none(),
            }
        })
        .collect()
}

/// One tier's score and cost for the value chart. The score is
/// `100 + 10 × z` from that tier's observations only. Board shrinkage is
/// not applied: shrinkage is defined for the whole board, and a per-tier
/// board would be a different statistic.
pub(super) struct TierPoint {
    pub(super) tier: &'static str,
    pub(super) tasks: usize,
    pub(super) score: Option<f64>,
    pub(super) cost_per_task: Option<f64>,
    pub(super) cost_coverage: f64,
    pub(super) cost_unknown: bool,
}

pub(super) fn tier_points(row: &ModelRow, scales: &[Option<f64>; MEASURE_COUNT]) -> Vec<TierPoint> {
    by_tier(&row.records)
        .into_iter()
        .map(|(tier, records)| {
            let tasks = records.len();
            let tier_totals = totals(records.iter().map(|record| &record.observed));
            let score = composite(&tier_totals, scales, review_supported(&tier_totals))
                .map(|z| 100.0 + 10.0 * z);
            let priced: Vec<f64> = row
                .costs
                .iter()
                .filter_map(|(cost_tier, cost)| (*cost_tier == tier).then_some(*cost).flatten())
                .collect();
            let coverage = if tasks == 0 {
                0.0
            } else {
                priced.len() as f64 / tasks as f64
            };
            let per_task =
                (!priced.is_empty()).then(|| priced.iter().sum::<f64>() / priced.len() as f64);
            TierPoint {
                tier,
                tasks,
                score,
                cost_per_task: per_task,
                cost_coverage: coverage,
                cost_unknown: coverage < MIN_COST_COVERAGE || per_task.is_none(),
            }
        })
        .collect()
}
