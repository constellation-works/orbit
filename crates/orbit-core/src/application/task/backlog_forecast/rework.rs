//! Frozen regression bundles and one generation of forecast-only work.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use orbit_common::fs::selector::Selector;
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, Task, TaskHistoryEntry, TaskRelationType,
    TaskStatus, is_no_diff_expected,
};
use serde_json::{Value, json};

use crate::application::task::paths::{CONTEXT_FILES_PRUNED_EVENT, pruned_selectors_from_note};

use super::correction::{self, Correction};
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
    /// The completed task; a recovered footprint replaces its selectors in
    /// this forecast-only copy, never in the stored record.
    pub(super) task: Task,
    pub(super) delivery: Delivery,
    pub(super) item: Item,
    pub(super) resolved: model::Resolved,
    /// The footprint came from durable history, not current selectors.
    pub(super) recovered: bool,
}

/// Why a regression fix in the delivery lookback cannot be drawn. Each is
/// real rework whose evidence is incomplete, so it withholds support from
/// every pool its origins would have joined instead of being dropped.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FixGap {
    /// The fix completed without a measured delivery.
    UnmeasuredDelivery,
    /// Neither current selectors nor durable history give a footprint.
    FootprintUnrecoverable,
    /// The origin is done without a durable completion: no detection lag.
    OriginCompletionMissing,
}

impl FixGap {
    const ALL: [Self; 3] = [
        Self::UnmeasuredDelivery,
        Self::FootprintUnrecoverable,
        Self::OriginCompletionMissing,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::UnmeasuredDelivery => "unmeasured_delivery",
            Self::FootprintUnrecoverable => "footprint_unrecoverable",
            Self::OriginCompletionMissing => "origin_completion_missing",
        }
    }
}

/// One unsupported fix observation and the origins whose cells it belongs to.
pub(super) struct HistoryGap {
    pub(super) reason: FixGap,
    pub(super) origins: Vec<String>,
}

/// What a `regression_from` link's target proves about the fix.
enum OriginEvidence {
    /// A durable completion dates the detection lag.
    Completed,
    /// Done, but no durable completion survives.
    CompletionMissing,
    /// No record in this workspace.
    Absent,
    /// The record never completed. The model only draws regressions of
    /// completed work, so neither this nor an absent origin is a sample.
    NotCompleted,
}

fn origin_evidence(
    target: &str,
    tasks: &HashMap<String, Task>,
    done_at: &HashMap<String, DateTime<Utc>>,
) -> OriginEvidence {
    if done_at.contains_key(target) {
        return OriginEvidence::Completed;
    }
    match tasks.get(target) {
        None => OriginEvidence::Absent,
        Some(task) if task.status == TaskStatus::Done => OriginEvidence::CompletionMissing,
        Some(_) => OriginEvidence::NotCompleted,
    }
}

/// An origin's bundle joins `item`'s complexity × crew pool.
fn origin_in_cell(origin: &Task, item: &Item) -> bool {
    origin.complexity.map(|c| c.as_str()) == Some(item.complexity.as_str())
        && origin
            .crew
            .as_ref()
            .is_some_and(|crew| item.crews.contains(crew))
}

pub(super) struct RegressionRework {
    pub(super) donors: Vec<ForecastDonor>,
    pub(super) bundles: Vec<RegressionBundle>,
    pub(super) pools: Vec<Vec<usize>>,
    pub(super) pool_basis: Vec<&'static str>,
    pub(super) probabilities: Vec<Option<f64>>,
    pub(super) prior_basis: Vec<&'static str>,
    pub(super) risks: Vec<Value>,
    /// Unsupported fix observations in the lookback.
    pub(super) gaps: Vec<HistoryGap>,
    /// Per item, the gaps relevant to the pool it samples.
    pub(super) pool_gaps: Vec<Vec<usize>>,
    /// Fix links that are not regression samples, by reason.
    pub(super) excluded_links: BTreeMap<&'static str, usize>,
    pub(super) known_origins: BTreeMap<String, BTreeSet<String>>,
}

impl RegressionRework {
    /// Construct bundles from distinct measured fixes; links to multiple
    /// culprits retain one donor identity for per-scenario deduplication.
    /// Every fix completed in the lookback is classified first, and each
    /// item's pool is charged only the gaps whose origins it would sample.
    pub(super) fn build_regression_bundles(
        &mut self,
        tasks: &HashMap<String, Task>,
        done_at: &HashMap<String, DateTime<Utc>>,
        history: &[Delivery],
        lookback_start: DateTime<Utc>,
        items: &[Item],
    ) {
        self.classify_fix_history(tasks, done_at, history, lookback_start);
        let mut bundles: BTreeMap<String, Vec<RegressionFix>> = BTreeMap::new();
        for (donor, row) in self.donors.iter().enumerate() {
            for link in &row.task.relations {
                if link.relation_type != TaskRelationType::RegressionFrom {
                    continue;
                }
                let Some(done) = done_at.get(link.target.as_str()) else {
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
                        origin_in_cell(task, item).then_some(index)
                    })
                    .collect();
                if matching.len() >= model::MIN_CELL_SAMPLES {
                    (matching, "complexity_crew")
                } else {
                    ((0..self.bundles.len()).collect(), "workspace")
                }
            })
            .unzip();
        self.pool_gaps = items
            .iter()
            .zip(&self.pool_basis)
            .map(|(item, basis)| {
                (0..self.gaps.len())
                    .filter(|gap| {
                        *basis == "workspace"
                            || self.gaps[*gap].origins.iter().any(|origin| {
                                tasks
                                    .get(origin)
                                    .is_some_and(|origin| origin_in_cell(origin, item))
                            })
                    })
                    .collect()
            })
            .collect();
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

    /// Sort each fix completed in the lookback into a drawable donor, a
    /// non-sample (every link names an absent or never-completed origin)
    /// or a gap. A donor's link to an origin without durable completion is
    /// its own gap; its other links still form bundles.
    fn classify_fix_history(
        &mut self,
        tasks: &HashMap<String, Task>,
        done_at: &HashMap<String, DateTime<Utc>>,
        history: &[Delivery],
        lookback_start: DateTime<Utc>,
    ) {
        let mut fixes: Vec<&Task> = tasks
            .values()
            .filter(|task| {
                done_at
                    .get(task.id.as_str())
                    .is_some_and(|at| *at >= lookback_start)
            })
            .collect();
        fixes.sort_by(|left, right| left.id.cmp(&right.id));
        for fix in fixes {
            let mut origins = Vec::new();
            let mut lagless = Vec::new();
            for link in &fix.relations {
                if link.relation_type != TaskRelationType::RegressionFrom {
                    continue;
                }
                let target = link.target.to_string();
                match origin_evidence(&target, tasks, done_at) {
                    OriginEvidence::Completed => origins.push(target),
                    OriginEvidence::CompletionMissing => {
                        lagless.push(target.clone());
                        origins.push(target);
                    }
                    OriginEvidence::Absent => {
                        *self.excluded_links.entry("origin_absent").or_default() += 1;
                    }
                    OriginEvidence::NotCompleted => {
                        *self
                            .excluded_links
                            .entry("origin_not_completed")
                            .or_default() += 1;
                    }
                }
            }
            if origins.is_empty() {
                continue;
            }
            if self.donors.iter().any(|donor| donor.task.id == fix.id) {
                self.gaps
                    .extend(lagless.into_iter().map(|origin| HistoryGap {
                        reason: FixGap::OriginCompletionMissing,
                        origins: vec![origin],
                    }));
                continue;
            }
            let reason = if history
                .iter()
                .any(|delivery| delivery.task_id == fix.id.as_str())
            {
                FixGap::FootprintUnrecoverable
            } else {
                FixGap::UnmeasuredDelivery
            };
            self.gaps.push(HistoryGap { reason, origins });
        }
    }

    /// The item's pool is drawable and no unsupported fix belongs to it.
    pub(super) fn history_supported(&self, index: usize) -> bool {
        !self.pools[index].is_empty() && self.pool_gaps[index].is_empty()
    }

    /// Gaps relevant to any of `members`' pools, counted once by reason.
    pub(super) fn member_gaps(&self, members: &[usize]) -> Value {
        let relevant: BTreeSet<usize> = members
            .iter()
            .flat_map(|index| self.pool_gaps[*index].iter().copied())
            .collect();
        self.gap_counts(relevant.into_iter())
    }

    fn gap_counts(&self, gaps: impl Iterator<Item = usize>) -> Value {
        let mut counts: serde_json::Map<String, Value> = FixGap::ALL
            .iter()
            .map(|reason| (reason.key().to_string(), json!(0)))
            .collect();
        for gap in gaps {
            let key = self.gaps[gap].reason.key();
            counts[key] = json!(counts[key].as_u64().unwrap_or(0) + 1);
        }
        Value::Object(counts)
    }

    pub(super) fn regression_inputs_supported(&self, included: &[bool]) -> bool {
        included
            .iter()
            .take(self.probabilities.len())
            .enumerate()
            .all(|(index, included)| {
                !included || (self.probabilities[index].is_some() && self.history_supported(index))
            })
    }

    pub(super) fn regression_history_report(&self) -> Value {
        json!({
            "generation_limit": 1,
            "missing_fix_history": self.gaps.len(),
            "missing_by_reason": self.gap_counts(0..self.gaps.len()),
            "recovered_footprints": self.donors.iter().filter(|donor| donor.recovered).count(),
            "excluded": {
                "origin_absent": self.excluded_links.get("origin_absent").copied().unwrap_or(0),
                "origin_not_completed": self.excluded_links.get("origin_not_completed").copied().unwrap_or(0),
            },
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
    is_no_diff_expected(&task.tags) || usable_selectors(&task.context_files)
}

fn usable_selectors(selectors: &[String]) -> bool {
    selectors.iter().any(|raw| {
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

/// The footprint a completed fix's durable history recorded: `file:`
/// selectors delivery widened over changed paths up to its completion
/// `done`, and declarations a retired pruning pass dropped. Empty when
/// neither survives; nothing is inferred.
pub(super) fn recorded_fix_footprint(
    history: &[TaskHistoryEntry],
    done: DateTime<Utc>,
) -> Vec<String> {
    let mut selectors: Vec<String> = Vec::new();
    for entry in history {
        let recorded = if entry.event == CONTEXT_FILES_PRUNED_EVENT {
            entry
                .note
                .as_deref()
                .map(pruned_selectors_from_note)
                .unwrap_or_default()
        } else if entry.event == CONTEXT_FILES_WIDENED_EVENT && entry.at <= done {
            ContextFilesWidening::from_entry(entry)
                .map(|widening| widening.selectors)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        for selector in recorded {
            if !selectors.contains(&selector) {
                selectors.push(selector);
            }
        }
    }
    selectors
}

/// Sample whole bundles once per iteration; both scenarios replay these
/// draws. Donors already in the initial cohort cannot be duplicated.
pub(super) fn draw_regression_bundles(
    rework: &RegressionRework,
    items: &[Item],
    included: &[bool],
    episodes: &[Episode],
    rng: &mut Rng,
    correction: &Correction,
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
                correction,
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
    correction: &Correction,
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
    correction::apply_script(correction, &donor.item.complexity, &mut script);
    ForecastGeneratedNode {
        template,
        origin,
        lag,
        script,
        hours: donor.delivery.hours,
    }
}

/// Measured completed deliveries with a usable latent footprint: the
/// current selectors, else the fix footprint `recorded` from durable
/// history. Class pooling is separate from duration/review-mode resolution.
pub(super) fn collect_forecast_donors(
    tasks: &HashMap<String, Task>,
    recorded: &HashMap<String, Vec<String>>,
    history: &[Delivery],
    episodes: &[Episode],
    waits: &[AwayWait],
) -> Vec<ForecastDonor> {
    history
        .iter()
        .filter_map(|delivery| {
            let current = tasks.get(&delivery.task_id)?;
            let (task, recovered) = if usable_footprint(current) {
                (current.clone(), false)
            } else {
                let selectors = recorded
                    .get(&delivery.task_id)
                    .filter(|selectors| usable_selectors(selectors))?;
                (
                    Task {
                        context_files: selectors.clone(),
                        ..current.clone()
                    },
                    true,
                )
            };
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
                task,
                delivery: delivery.clone(),
                item,
                resolved,
                recovered,
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
    correction: &Correction,
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
            correction,
        ));
        at -= rng.unit().ln() / (total / 24.0);
    }
    false
}
