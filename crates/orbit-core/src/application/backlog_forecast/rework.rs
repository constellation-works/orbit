//! Frozen regression bundles and one generation of forecast-only work.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::Instant;

use chrono::{DateTime, Utc};
use orbit_common::fs::selector::Selector;
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, LegacyContextPruningRecord, Task,
    TaskHistoryEntry, TaskRelationType, TaskStatus, is_no_diff_expected,
};
use serde_json::{Value, json};

use crate::application::statistics::SplitMix64;

use super::correction::{self, Correction, ScriptCorrection};
use super::model::{self, Delivery, Draw, Item, Script};
use super::outcomes::{AwayWait, Episode};
use super::schedule::HORIZON_HOURS;

/// Bound generated nodes independently of the event bound.
pub(super) const MAX_GENERATED: usize = 10_000;

/// Count an isolated test fixture's server substitutes for
/// [`MAX_GENERATED`]. Only dev-dependency builds with `test-support`
/// recognize this variable.
#[cfg(feature = "test-support")]
pub const MAX_GENERATED_ENV: &str = "ORBIT_TEST_FORECAST_MAX_GENERATED";

/// The generation bound: [`MAX_GENERATED`] unless a test fixture
/// substituted another through [`MAX_GENERATED_ENV`].
pub(super) fn max_generated() -> usize {
    #[cfg(feature = "test-support")]
    if let Some(count) = std::env::var(MAX_GENERATED_ENV)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|count: &usize| *count > 0)
    {
        return count;
    }
    MAX_GENERATED
}

/// The tag a fix record carries when its repair ran inside its origin's own
/// delivery (typically that origin's landing review) rather than as a
/// separate delivery.
pub(super) const DEFECT_RECORD_TAG: &str = "defect-record";

/// The largest share of a pool's fix observations that may be gaps while its
/// adjusted totals are still reported, under a `bounded_gap` qualifier.
pub(super) const MAX_BOUNDED_GAP_SHARE: f64 = 0.10;

/// Each arrival source class's donor indexes.
pub(super) type ArrivalPools = BTreeMap<&'static str, Vec<usize>>;

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
    /// The correction reference sample for this forecast, computed on first use.
    script_correction: OnceCell<ScriptCorrection>,
    /// The footprint came from durable history, not current selectors.
    pub(super) recovered: bool,
}

/// Why a regression fix in the delivery lookback cannot be drawn. Each is
/// real rework whose evidence is incomplete, so it counts against every pool
/// its origins would have joined instead of being dropped: that pool is
/// withheld unless its gaps are bounded (see
/// [`RegressionRework::history_bounded`]).
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
    /// Per item, the distinct drawable fixes in the pool it samples.
    pub(super) pool_fixes: Vec<usize>,
    /// Defect-record fixes repaired inside their origin's measured delivery:
    /// their spend is already in that origin's history sample.
    pub(super) in_origin_review: usize,
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
        self.pool_fixes = self
            .pools
            .iter()
            .map(|pool| {
                pool.iter()
                    .flat_map(|bundle| self.bundles[*bundle].fixes.iter().map(|fix| fix.donor))
                    .collect::<BTreeSet<usize>>()
                    .len()
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
            let measured = |id: &str| history.iter().any(|delivery| delivery.task_id == id);
            let reason = if measured(fix.id.as_str()) {
                FixGap::FootprintUnrecoverable
            } else if repaired_in_origin(fix, &origins, &measured) {
                self.in_origin_review += 1;
                continue;
            } else {
                FixGap::UnmeasuredDelivery
            };
            self.gaps.push(HistoryGap { reason, origins });
        }
    }

    /// The item's pool is drawable and either no unsupported fix belongs to
    /// it or its gaps are bounded.
    pub(super) fn history_supported(&self, index: usize) -> bool {
        !self.pools[index].is_empty()
            && (self.pool_gaps[index].is_empty() || self.history_bounded(index))
    }

    /// The item's pool holds gaps, but has at least
    /// [`model::MIN_CELL_SAMPLES`] bundles and at most
    /// [`MAX_BOUNDED_GAP_SHARE`] of its fix observations are gaps, so its
    /// adjusted totals are reported with a `bounded_gap` qualifier instead of
    /// withheld. The gaps are disclosed, never counted as zero rework.
    pub(super) fn history_bounded(&self, index: usize) -> bool {
        let gaps = self.pool_gaps[index].len();
        gaps > 0
            && self.pools[index].len() >= model::MIN_CELL_SAMPLES
            && gaps as f64 / (gaps + self.pool_fixes[index]) as f64 <= MAX_BOUNDED_GAP_SHARE
    }

    /// The bounded-gap qualifier over `members`' bounded pools: the distinct
    /// gaps and drawable fixes in them, the gaps' share and reasons; null
    /// when no member samples a bounded pool.
    pub(super) fn member_bounded_gap(&self, members: &[usize]) -> Value {
        let bounded: Vec<usize> = members
            .iter()
            .copied()
            .filter(|index| self.history_bounded(*index))
            .collect();
        if bounded.is_empty() {
            return Value::Null;
        }
        let gaps: BTreeSet<usize> = bounded
            .iter()
            .flat_map(|index| self.pool_gaps[*index].iter().copied())
            .collect();
        let fixes = bounded
            .iter()
            .flat_map(|index| self.pools[*index].iter())
            .flat_map(|bundle| self.bundles[*bundle].fixes.iter().map(|fix| fix.donor))
            .collect::<BTreeSet<usize>>()
            .len();
        let share = gaps.len() as f64 / (gaps.len() + fixes) as f64;
        json!({
            "tasks": bounded.len(),
            "gaps": gaps.len(),
            "drawn_fixes": fixes,
            "gap_share": (share * 1000.0).round() / 1000.0,
            "max_gap_share": MAX_BOUNDED_GAP_SHARE,
            "by_reason": self.gap_counts(gaps.into_iter()),
        })
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
            "recovered_in_origin_review": self.in_origin_review,
            "max_bounded_gap_share": MAX_BOUNDED_GAP_SHARE,
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

/// An unmeasured `defect-record` fix whose repair ran inside its origin's
/// delivery: every simulating origin is the task it was spawned from, and
/// each has a measured delivery, so the repair's spend is already inside
/// that origin's history sample. Drawing it again would double count it.
fn repaired_in_origin(fix: &Task, origins: &[String], measured: &dyn Fn(&str) -> bool) -> bool {
    fix.tags.iter().any(|tag| tag == DEFECT_RECORD_TAG)
        && origins.iter().all(|origin| {
            measured(origin)
                && fix.relations.iter().any(|link| {
                    link.relation_type == TaskRelationType::SpawnedFrom
                        && link.target.as_str() == origin.as_str()
                })
        })
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
        let recorded = if entry.event == CONTEXT_FILES_WIDENED_EVENT {
            if entry.at <= done {
                ContextFilesWidening::from_entry(entry)
                    .map(|widening| widening.selectors)
                    .unwrap_or_default()
            } else {
                Vec::new()
            }
        } else {
            LegacyContextPruningRecord::from_entry(entry)
                .map(|record| record.selectors)
                .unwrap_or_default()
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
    rng: &mut SplitMix64,
    correction: &Correction,
    deadline: Option<Instant>,
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
            if nodes.len() == max_generated() {
                return (nodes, true);
            }
            // The caller discards this iteration on an expired deadline.
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return (nodes, false);
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
    rng: &mut SplitMix64,
    correction: &Correction,
) -> ForecastGeneratedNode {
    let delivery = Draw {
        hours: Some(donor.delivery.hours),
        tokens: donor.delivery.tokens,
        usd: donor.delivery.usd,
    };
    let draw_script = |rng: &mut SplitMix64, script: &mut Script| {
        model::draw_forecast_attempts(
            rng,
            &donor.item,
            &donor.resolved,
            episodes,
            delivery,
            script,
        );
    };
    let adjustment = donor.script_correction.get_or_init(|| {
        correction.prepare_script_correction(
            &donor.item.complexity,
            model::FORECAST_SEED,
            draw_script,
        )
    });
    let mut script = Script::default();
    draw_script(rng, &mut script);
    correction::apply_script_correction(adjustment, &mut script);
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
                drain_hold: None,
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
                script_correction: OnceCell::new(),
                recovered,
            })
        })
        .collect()
}

/// Each arrival source class's donors, measured once per forecast.
pub(super) fn forecast_arrival_pools(rework: &RegressionRework) -> ArrivalPools {
    let mut pools = ArrivalPools::new();
    for (index, donor) in rework.donors.iter().enumerate() {
        pools
            .entry(super::arrival_source(&donor.task))
            .or_default()
            .push(index);
    }
    pools
}

/// Exogenous filings, drawn on demand up to the horizon. The event loop stops
/// consuming them at its first full drain (not during a detection gap), so
/// filings past that point are never generated.
pub(super) struct ForecastArrivalStream<'a> {
    rework: &'a RegressionRework,
    items: usize,
    rates: &'a BTreeMap<&'static str, f64>,
    pools: &'a ArrivalPools,
    total: f64,
    /// When the next undrawn filing arrives.
    next_at: f64,
}

impl<'a> ForecastArrivalStream<'a> {
    /// `None`, with no draw, when nothing arrives or no donor can stand in.
    pub(super) fn start(
        rework: &'a RegressionRework,
        items: usize,
        rates: &'a BTreeMap<&'static str, f64>,
        pools: &'a ArrivalPools,
        rng: &mut SplitMix64,
    ) -> Option<Self> {
        let total: f64 = rates.values().sum();
        if total <= 0.0 || rework.donors.is_empty() {
            return None;
        }
        Some(Self {
            rework,
            items,
            rates,
            pools,
            total,
            next_at: -rng.unit().ln() / (total / 24.0),
        })
    }

    /// Simulated hours up to which `nodes` holds every filing; unbounded once
    /// the stream has passed the horizon.
    pub(super) fn known_until(&self) -> f64 {
        if self.next_at <= HORIZON_HOURS {
            self.next_at
        } else {
            f64::INFINITY
        }
    }

    /// Draw filings until `nodes` holds `limit` nodes or the horizon passes.
    pub(super) fn extend_forecast_arrivals(
        &mut self,
        limit: usize,
        episodes: &[Episode],
        rng: &mut SplitMix64,
        correction: &Correction,
        nodes: &mut Vec<ForecastGeneratedNode>,
        deadline: Option<Instant>,
    ) {
        while self.next_at <= HORIZON_HOURS
            && nodes.len() < limit
            && deadline.is_none_or(|deadline| Instant::now() < deadline)
        {
            let pick = rng.unit() * self.total;
            let mut accumulated = 0.0;
            let class = self
                .rates
                .iter()
                .find_map(|(class, rate)| {
                    accumulated += rate;
                    (pick <= accumulated).then_some(*class)
                })
                .unwrap_or("filing");
            let donor = match self.pools.get(class) {
                Some(pool) if pool.len() >= model::MIN_CELL_SAMPLES => pool[rng.index(pool.len())],
                _ => rng.index(self.rework.donors.len()),
            };
            nodes.push(generated_node(
                &self.rework.donors[donor],
                self.items + donor,
                None,
                self.next_at,
                episodes,
                rng,
                correction,
            ));
            self.next_at -= rng.unit().ln() / (self.total / 24.0);
        }
    }
}
