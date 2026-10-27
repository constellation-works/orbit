//! Frozen regression bundles and one generation of forecast-only work.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use orbit_common::fs::selector::Selector;
use orbit_types::task::{Task, TaskRelationType, is_no_diff_expected};
use serde_json::{Value, json};

use super::model::{self, Delivery, Draw, Item, Rng, Script};
use super::outcomes::{AwayWait, Episode};
use super::schedule::HORIZON_HOURS;

/// Bound generated nodes independently of the event bound.
pub(super) const MAX_GENERATED: usize = 10_000;

/// A historical fix and its measured detection delay, in an origin bundle.
pub(super) struct RegressionFix {
    pub(super) donor: usize,
    pub(super) lag: f64,
}

pub(super) struct RegressionBundle {
    pub(super) origin: String,
    pub(super) fixes: Vec<RegressionFix>,
}

/// The historical task templates follow the initial items in admission.
pub(super) struct ForecastDonor {
    pub(super) task: Task,
    pub(super) delivery: Delivery,
    pub(super) item: Item,
    pub(super) resolved: model::Resolved,
}

pub(super) struct RegressionRework {
    pub(super) donors: Vec<ForecastDonor>,
    pub(super) bundles: Vec<RegressionBundle>,
    pub(super) pools: Vec<Vec<usize>>,
    pub(super) pool_basis: Vec<&'static str>,
    pub(super) probabilities: Vec<Option<f64>>,
    pub(super) prior_basis: Vec<&'static str>,
    pub(super) risks: Vec<Value>,
    pub(super) missing_fix_history: usize,
    pub(super) known_origins: BTreeMap<String, BTreeSet<String>>,
}

impl RegressionRework {
    /// Construct bundles from distinct measured fixes; links to multiple
    /// culprits retain one donor identity for per-scenario deduplication.
    pub(super) fn build_regression_bundles(
        &mut self,
        tasks: &HashMap<String, Task>,
        done_at: &HashMap<String, DateTime<Utc>>,
        items: &[Item],
    ) {
        let mut bundles: BTreeMap<String, Vec<RegressionFix>> = BTreeMap::new();
        for (donor, row) in self.donors.iter().enumerate() {
            for link in &row.task.relations {
                if link.relation_type != TaskRelationType::RegressionFrom {
                    continue;
                }
                let Some(done) = done_at.get(link.target.as_str()) else {
                    self.missing_fix_history += 1;
                    continue;
                };
                bundles
                    .entry(link.target.to_string())
                    .or_default()
                    .push(RegressionFix {
                        donor,
                        lag: super::outcomes::hours_between(*done, row.task.created_at),
                    });
            }
        }
        self.bundles = bundles
            .into_iter()
            .map(|(origin, fixes)| RegressionBundle { origin, fixes })
            .collect();
        (self.pools, self.pool_basis) = items
            .iter()
            .map(|item| {
                let matching: Vec<usize> = self
                    .bundles
                    .iter()
                    .enumerate()
                    .filter_map(|(index, bundle)| {
                        let task = tasks.get(&bundle.origin)?;
                        (task.complexity.map(|c| c.as_str()) == Some(item.complexity.as_str())
                            && task
                                .crew
                                .as_ref()
                                .is_some_and(|crew| item.crews.contains(crew)))
                        .then_some(index)
                    })
                    .collect();
                if matching.len() >= model::MIN_CELL_SAMPLES {
                    (matching, "complexity_crew")
                } else {
                    ((0..self.bundles.len()).collect(), "workspace")
                }
            })
            .unzip();
        for item in items {
            let Some(task) = tasks.get(&item.task_id) else {
                continue;
            };
            for link in &task.relations {
                if link.relation_type == TaskRelationType::RegressionFrom {
                    self.known_origins
                        .entry(link.target.to_string())
                        .or_default()
                        .insert(item.task_id.clone());
                }
            }
        }
    }

    pub(super) fn regression_inputs_supported(&self, included: &[bool]) -> bool {
        (!included
            .iter()
            .take(self.probabilities.len())
            .any(|included| *included)
            || self.missing_fix_history == 0)
            && included
                .iter()
                .take(self.probabilities.len())
                .enumerate()
                .all(|(index, included)| {
                    !included
                        || (self.probabilities[index].is_some() && !self.pools[index].is_empty())
                })
    }

    pub(super) fn regression_history_report(&self) -> Value {
        json!({
            "generation_limit": 1,
            "missing_fix_history": self.missing_fix_history,
            "history_fixes": self.donors.iter().filter(|d| d.task.relations.iter().any(|l| l.relation_type == TaskRelationType::RegressionFrom)).count(),
            "history_bundles": self.bundles.len(),
            "history_thin": self.bundles.len() < model::MIN_CELL_SAMPLES,
        })
    }
}

/// One generated node, released after its origin finishes, or at an
/// absolute exogenous filing time. Only initial tasks spawn regressions.
pub(super) struct ForecastGeneratedNode {
    pub(super) template: usize,
    pub(super) origin: Option<usize>,
    pub(super) lag: f64,
    pub(super) script: Script,
    pub(super) hours: f64,
}

fn usable_footprint(task: &Task) -> bool {
    is_no_diff_expected(&task.tags)
        || task.context_files.iter().any(|raw| {
            let raw = raw.trim();
            if raw.is_empty() {
                return false;
            }
            if !raw.contains(':') || raw.starts_with('/') {
                return true;
            }
            matches!(
                raw.parse::<Selector>(),
                Ok(Selector::File { .. } | Selector::Dir { .. } | Selector::Symbol { .. })
            )
        })
}

/// Sample whole bundles once per iteration; both scenarios replay these
/// draws. Donors already in the initial cohort cannot be duplicated.
pub(super) fn draw_regression_bundles(
    rework: &RegressionRework,
    items: &[Item],
    included: &[bool],
    episodes: &[Episode],
    rng: &mut Rng,
) -> (Vec<ForecastGeneratedNode>, bool) {
    let mut nodes = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, item) in items.iter().enumerate() {
        if !included[index] {
            continue;
        }
        let Some(p) = rework.probabilities[index] else {
            continue;
        };
        let pool = &rework.pools[index];
        if rng.unit() > p || pool.is_empty() {
            continue;
        }
        let bundle = &rework.bundles[pool[rng.index(pool.len())]];
        let mut known = rework.known_origins.get(&item.task_id).map_or(0, |fixes| {
            fixes
                .iter()
                .filter(|id| {
                    items.iter().enumerate().any(|(index, item)| {
                        included[index] && item.task_id.as_str() == id.as_str()
                    })
                })
                .count()
        });
        for fix in &bundle.fixes {
            let donor = &rework.donors[fix.donor];
            if items
                .iter()
                .enumerate()
                .any(|(index, item)| included[index] && item.task_id == donor.task.id)
                || !seen.insert(fix.donor)
            {
                continue;
            }
            if known > 0 {
                known -= 1;
                continue;
            }
            if nodes.len() == MAX_GENERATED {
                return (nodes, true);
            }
            nodes.push(generated_node(
                donor,
                items.len() + fix.donor,
                Some(index),
                fix.lag,
                episodes,
                rng,
            ));
        }
    }
    (nodes, false)
}

fn generated_node(
    donor: &ForecastDonor,
    template: usize,
    origin: Option<usize>,
    lag: f64,
    episodes: &[Episode],
    rng: &mut Rng,
) -> ForecastGeneratedNode {
    let mut script = Script::default();
    model::draw_forecast_attempts(
        rng,
        &donor.item,
        &donor.resolved,
        episodes,
        Draw {
            hours: Some(donor.delivery.hours),
            tokens: donor.delivery.tokens,
            usd: donor.delivery.usd,
        },
        &mut script,
    );
    ForecastGeneratedNode {
        template,
        origin,
        lag,
        script,
        hours: donor.delivery.hours,
    }
}

/// Measured completed deliveries with a usable latent footprint. Class
/// pooling is separate from duration/review-mode resolution.
pub(super) fn collect_forecast_donors(
    tasks: &HashMap<String, Task>,
    history: &[Delivery],
    episodes: &[Episode],
    waits: &[AwayWait],
) -> Vec<ForecastDonor> {
    history
        .iter()
        .filter_map(|delivery| {
            let task = tasks.get(&delivery.task_id)?;
            if !usable_footprint(task) {
                return None;
            }
            let item = Item {
                task_id: task.id.clone(),
                title: task.title.clone(),
                complexity: delivery.complexity.clone(),
                crews: delivery.crew.iter().cloned().collect(),
                review_mode: delivery.review_mode,
                review_mode_source: "historical_donor".into(),
                in_progress: false,
                proposed: false,
                blocked: false,
                age_hours: 0.0,
                spent: model::Spent::default(),
            };
            let resolved = model::resolve_forecast_item(&item, history, episodes, waits);
            Some(ForecastDonor {
                task: task.clone(),
                delivery: delivery.clone(),
                item,
                resolved,
            })
        })
        .collect()
}

/// Draw a bounded exogenous stream up to the horizon; the event loop stops
/// consuming it at its first full drain, but not during a detection gap.
pub(super) fn draw_forecast_arrivals(
    rework: &RegressionRework,
    items: usize,
    rates: &BTreeMap<&'static str, f64>,
    episodes: &[Episode],
    rng: &mut Rng,
    nodes: &mut Vec<ForecastGeneratedNode>,
) -> bool {
    let total: f64 = rates.values().sum();
    if total <= 0.0 || rework.donors.is_empty() {
        return false;
    }
    let mut at = -rng.unit().ln() / (total / 24.0);
    while at <= HORIZON_HOURS {
        if nodes.len() == MAX_GENERATED {
            return true;
        }
        let pick = rng.unit() * total;
        let mut accumulated = 0.0;
        let class = rates
            .iter()
            .find_map(|(class, rate)| {
                accumulated += rate;
                (pick <= accumulated).then_some(*class)
            })
            .unwrap_or("filing");
        let matching: Vec<usize> = rework
            .donors
            .iter()
            .enumerate()
            .filter_map(|(index, donor)| {
                (super::arrival_source(&donor.task) == class).then_some(index)
            })
            .collect();
        let donor = if matching.len() >= model::MIN_CELL_SAMPLES {
            matching[rng.index(matching.len())]
        } else {
            rng.index(rework.donors.len())
        };
        nodes.push(generated_node(
            &rework.donors[donor],
            items + donor,
            None,
            at,
            episodes,
            rng,
        ));
        at -= rng.unit().ln() / (total / 24.0);
    }
    false
}
