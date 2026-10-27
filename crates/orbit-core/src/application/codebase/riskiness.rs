//! Per-task riskiness: the observed chance that a task, once done, is named
//! by at least one `regression_from` link, estimated from its declared
//! footprint, crew and complexity.
//!
//! The estimate reads the same windowed regression facts as the
//! regression-risk marks ([`super::risk`]) and the Regressions ranking
//! ([`regression_rows`]): every task done in the trailing
//! `workflow.regression_risk.window_days` is one observation, regressed when
//! any link names it. `min_support` is the shared minimum; the marks'
//! threshold and file cap do not apply.
//!
//! 1. Each `file:`/`symbol:` selector is its path; a `dir:` selector rolls up
//!    every footprint key under it, counting each historical task once.
//!    Module and command selectors carry no path and are counted as
//!    unsupported.
//! 2. With N done tasks and R regressed, the base rate is
//!    `p0 = (R + 1) / (N + 2)`. An anchor touched by `n_f >= min_support`
//!    tasks, `r_f` of them regressed, has the shrunk rate
//!    `p_f = (r_f + min_support * p0) / (n_f + min_support)`.
//! 3. Supported anchors combine by their maximum rate; the top three are the
//!    driving files.
//! 4. One joint crew × complexity adjustment shifts the odds:
//!    `logistic(logit(max p_f) + logit(p_g) - logit(p0))`, where `p_g` is
//!    that group's rate shrunk the same way, applied only when both the group
//!    and the workspace have at least `max(20, min_support)` observations. A
//!    task without a crew averages over its complexity pool's positive
//!    weights, unadjusted for members without support.
//! 5. A numeric estimate needs N and one anchor at `min_support`. Otherwise
//!    the state names why: `no_footprint`, `unsupported_footprint` or
//!    `insufficient_history`, never a low probability.
//!
//! The estimate is derived, not stored on the task; [`TaskRiskinessModel`]
//! is built once per scan and estimates any number of tasks without another
//! history read. The backlog forecast consumes [`TaskRiskiness::probability`]
//! unchanged.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskComplexity};
use serde::Serialize;

use super::facts::{Anchor, CodebaseFacts};
use super::regression::RegressionFilter;
use super::sections::{Params, Rollup, regression_rows};
use crate::OrbitRuntime;

/// Revision of the estimator below. Bump it whenever a change alters an
/// estimate for the same facts.
pub const TASK_RISKINESS_ESTIMATOR_VERSION: u32 = 1;
/// Fewest observations a crew × complexity group and the workspace need
/// before the group adjusts an estimate, when `min_support` is lower.
const GROUP_MIN_SUPPORT: usize = 20;
/// Driving files one estimate lists.
const DRIVING_FILES: usize = 3;
/// The complexity tiers whose crew pools an unassigned task averages over.
const POOL_TIERS: [TaskComplexity; 4] = [
    TaskComplexity::Low,
    TaskComplexity::Medium,
    TaskComplexity::Hard,
    TaskComplexity::XHard,
];

/// Whether a task has an estimate, and why not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskinessState {
    /// A probability from at least one supported anchor.
    Estimated,
    /// The task declares no `context_files`.
    NoFootprint,
    /// The task's selectors name no path (module or command selectors only).
    UnsupportedFootprint,
    /// Paths are declared, but the workspace or every anchor is below
    /// `min_support`.
    InsufficientHistory,
}

/// How much of an estimated footprint had support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportState {
    /// Every selector is a supported anchor.
    Full,
    /// Some anchors are below `min_support`, or some selectors name no path.
    Partial,
}

/// The workspace observations behind every estimate.
#[derive(Debug, Clone, Serialize)]
pub struct RiskinessBase {
    /// Done tasks in the window: N.
    pub tasks: usize,
    /// Of them, regressed: R.
    pub regressed: usize,
    /// `(R + 1) / (N + 2)`.
    pub base_rate: f64,
}

/// One supported anchor of the task's footprint.
#[derive(Debug, Clone, Serialize)]
pub struct RiskinessAnchor {
    /// The path; a directory ends in `/`.
    pub path: String,
    /// The shrunk rate `p_f`.
    pub probability: f64,
    /// Distinct done tasks in the window that touched it: `n_f`.
    pub support: usize,
    /// Of them, regressed: `r_f`.
    pub regressed: usize,
}

/// How the crew × complexity adjustment was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdjustmentState {
    /// The task's own crew group adjusted the estimate.
    Applied,
    /// The estimate averages the task's complexity pool over its weights.
    PoolWeighted,
    /// No adjustment: `reason` says why.
    Omitted,
}

/// One crew × complexity group the adjustment read.
#[derive(Debug, Clone, Serialize)]
pub struct RiskinessGroup {
    pub crew: String,
    /// The crew's pool weight, for a pool-weighted estimate.
    pub weight: Option<u32>,
    /// Done tasks in the window by this crew at this complexity.
    pub tasks: usize,
    pub regressed: usize,
    /// The group's shrunk rate `p_g`, when it has support.
    pub rate: Option<f64>,
    /// Whether this group adjusted the estimate.
    pub applied: bool,
}

/// The crew × complexity adjustment and its basis.
#[derive(Debug, Clone, Serialize)]
pub struct RiskinessAdjustment {
    pub state: AdjustmentState,
    /// `no_complexity`, `no_crew`, `workspace_below_support`,
    /// `group_below_support`, `pool_below_support` or, for a pool-weighted
    /// estimate with some unsupported members, `pool_partial_support`.
    pub reason: Option<&'static str>,
    /// Observations each group and the workspace need: `max(20, min_support)`.
    pub min_support: usize,
    pub groups: Vec<RiskinessGroup>,
}

/// One task's riskiness, as the task API serves it.
#[derive(Debug, Clone, Serialize)]
pub struct TaskRiskiness {
    pub estimator_version: u32,
    pub state: RiskinessState,
    /// The adjusted estimate; `None` unless `state` is `estimated`.
    pub probability: Option<f64>,
    /// The maximum anchor rate before the crew × complexity adjustment.
    pub unadjusted_probability: Option<f64>,
    pub support_state: Option<SupportState>,
    pub window_days: u32,
    pub min_support: usize,
    /// Start of the window.
    pub since: String,
    /// When the facts were read: later regressions are not yet counted, and
    /// tasks done shortly before it have had little time to reveal any.
    pub observed_through: String,
    pub workspace: RiskinessBase,
    /// Distinct done tasks behind the supported anchors; never a sum of
    /// per-anchor counts.
    pub supporting_tasks: usize,
    /// Of them, regressed.
    pub supporting_regressed: usize,
    /// Distinct path anchors the footprint names.
    pub anchors: usize,
    /// Path anchors below `min_support`.
    pub uncovered_anchors: usize,
    /// Selectors that name no path.
    pub unsupported_selectors: usize,
    /// The highest-rate supported anchors, at most three.
    pub driving_files: Vec<RiskinessAnchor>,
    /// The crew the estimate resolved, when the task names one.
    pub crew: Option<String>,
    pub complexity: Option<String>,
    pub adjustment: Option<RiskinessAdjustment>,
}

/// Done tasks in one crew × complexity group.
#[derive(Debug, Default, Clone, Copy)]
struct GroupTally {
    tasks: usize,
    regressed: usize,
}

/// The windowed regression facts every task estimate reads, built from one
/// scan.
#[derive(Debug)]
pub struct TaskRiskinessModel {
    computed_at: DateTime<Utc>,
    since: DateTime<Utc>,
    window_days: u32,
    min_support: usize,
    /// Whether each done task in the window was regressed, by observation.
    regressed: Vec<bool>,
    /// Every file-rollup footprint key and the observations that touched it.
    keys: BTreeMap<String, Vec<usize>>,
    /// Observations by (crew, complexity).
    groups: BTreeMap<(String, String), GroupTally>,
    /// Each complexity tier's crew pool: names with positive weights.
    pools: BTreeMap<&'static str, Vec<(String, u32)>>,
}

impl OrbitRuntime {
    /// Read the windowed regression facts once and build the model that
    /// estimates every task's riskiness from them, under the configured
    /// `workflow.regression_risk` window and minimum support and this
    /// host's complexity crew pools.
    pub fn task_riskiness_model(&self) -> Result<TaskRiskinessModel, OrbitError> {
        let settings = self.regression_risk_settings();
        let since = Utc::now() - Duration::days(i64::from(settings.window_days));
        let facts = super::facts::scan_regressions(self, since)?;
        let pools = POOL_TIERS
            .into_iter()
            .map(|tier| {
                let members = self
                    .configured_complexity_pool(tier)
                    .unwrap_or_else(|error| {
                        tracing::warn!(%tier, %error, "riskiness: crew pool unreadable");
                        None
                    })
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|candidate| candidate.weight > 0)
                    .map(|candidate| (candidate.crew.name, candidate.weight))
                    .collect();
                (tier.as_str(), members)
            })
            .collect();
        Ok(TaskRiskinessModel::build(
            &facts,
            since,
            settings.window_days,
            settings.min_support as usize,
            pools,
        ))
    }
}

impl TaskRiskinessModel {
    /// A supported prior for forecast work lacking a footprint estimate.
    /// This does not turn the task's UI estimate into a number.
    pub(crate) fn forecast_regression_prior(&self, task: &Task) -> (Option<f64>, &'static str) {
        let n = self.regressed.len();
        if n < self.min_support {
            return (None, "unsupported");
        }
        let r = self.regressed.iter().filter(|flag| **flag).count();
        let p0 = (r as f64 + 1.0) / (n as f64 + 2.0);
        if let (Some(crew), Some(complexity)) = (
            task.crew.as_ref().or(task.implemented_by.as_ref()),
            task.complexity.filter(|c| c.is_assessed()),
        ) && let Some(group) = self.groups.get(&(crew.clone(), complexity.as_str().into()))
            && group.tasks >= self.min_support
        {
            let m = self.min_support as f64;
            return (
                Some((group.regressed as f64 + m * p0) / (group.tasks as f64 + m)),
                "crew_complexity",
            );
        }
        (Some(p0), "workspace")
    }

    fn build(
        facts: &CodebaseFacts,
        since: DateTime<Utc>,
        window_days: u32,
        min_support: usize,
        pools: BTreeMap<&'static str, Vec<(String, u32)>>,
    ) -> Self {
        let params = Params {
            since: Some(since),
            min_support: min_support.max(1),
            rollup: Rollup::File,
            filter: RegressionFilter::default(),
        };
        let (rows, _) = regression_rows(facts, &params);
        let mut observation: HashMap<&str, usize> = HashMap::new();
        let mut regressed = Vec::new();
        let mut groups: BTreeMap<(String, String), GroupTally> = BTreeMap::new();
        for task in &facts.tasks {
            if !task.done_at.is_some_and(|at| params.within(at)) {
                continue;
            }
            let was_regressed = params.filter.regressed(task);
            observation.insert(task.id.as_str(), regressed.len());
            regressed.push(was_regressed);
            if let (Some(crew), Some(complexity)) = (&task.crew, &task.complexity) {
                let tally = groups
                    .entry((crew.clone(), complexity.clone()))
                    .or_default();
                tally.tasks += 1;
                tally.regressed += usize::from(was_regressed);
            }
        }
        let keys = rows
            .into_iter()
            .map(|(key, row)| {
                let touching = row
                    .touching
                    .iter()
                    .filter_map(|task| observation.get(task.id.as_str()).copied())
                    .collect();
                (key, touching)
            })
            .collect();
        Self {
            computed_at: facts.computed_at,
            since,
            window_days,
            min_support: params.min_support,
            regressed,
            keys,
            groups,
            pools,
        }
    }

    /// The riskiness of `task` from this model's facts.
    pub fn estimate(&self, task: &Task) -> TaskRiskiness {
        let m = self.min_support;
        let population = self.regressed.len();
        let regressed = self.regressed.iter().filter(|flag| **flag).count();
        let p0 = (regressed as f64 + 1.0) / (population as f64 + 2.0);
        let shrink = |r: usize, n: usize| (r as f64 + m as f64 * p0) / (n as f64 + m as f64);

        let selectors: Vec<&str> = task
            .context_files
            .iter()
            .map(|selector| selector.trim())
            .filter(|selector| !selector.is_empty())
            .collect();
        let mut anchors = BTreeSet::new();
        let mut unsupported_selectors = 0;
        for selector in &selectors {
            match Anchor::parse(selector) {
                Some(anchor) => {
                    anchors.insert(anchor);
                }
                None => unsupported_selectors += 1,
            }
        }

        let mut supported = Vec::new();
        let mut supporting = BTreeSet::new();
        let mut uncovered_anchors = 0;
        for anchor in &anchors {
            let touching = self.anchor_observations(anchor);
            let n = touching.len();
            if population < m || n < m {
                uncovered_anchors += 1;
                continue;
            }
            let r = touching.iter().filter(|at| self.regressed[**at]).count();
            supported.push(RiskinessAnchor {
                path: if anchor.dir {
                    format!("{}/", anchor.path)
                } else {
                    anchor.path.clone()
                },
                probability: shrink(r, n),
                support: n,
                regressed: r,
            });
            supporting.extend(touching);
        }
        supported.sort_by(|left, right| {
            right
                .probability
                .total_cmp(&left.probability)
                .then_with(|| right.support.cmp(&left.support))
                .then_with(|| left.path.cmp(&right.path))
        });

        let state = if selectors.is_empty() {
            RiskinessState::NoFootprint
        } else if anchors.is_empty() {
            RiskinessState::UnsupportedFootprint
        } else if supported.is_empty() {
            RiskinessState::InsufficientHistory
        } else {
            RiskinessState::Estimated
        };
        let crew = task
            .crew
            .as_deref()
            .map(str::trim)
            .filter(|crew| !crew.is_empty())
            .or(task.implemented_by.as_deref())
            .map(str::to_string);
        let complexity = task
            .complexity
            .filter(|complexity| complexity.is_assessed())
            .map(|complexity| complexity.as_str().to_string());

        let mut riskiness = TaskRiskiness {
            estimator_version: TASK_RISKINESS_ESTIMATOR_VERSION,
            state,
            probability: None,
            unadjusted_probability: None,
            support_state: None,
            window_days: self.window_days,
            min_support: m,
            since: self.since.to_rfc3339_opts(SecondsFormat::Secs, true),
            observed_through: self.computed_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            workspace: RiskinessBase {
                tasks: population,
                regressed,
                base_rate: p0,
            },
            supporting_tasks: supporting.len(),
            supporting_regressed: supporting.iter().filter(|at| self.regressed[**at]).count(),
            anchors: anchors.len(),
            uncovered_anchors,
            unsupported_selectors,
            driving_files: Vec::new(),
            crew: crew.clone(),
            complexity: complexity.clone(),
            adjustment: None,
        };
        let Some(top) = supported.first().map(|anchor| anchor.probability) else {
            return riskiness;
        };
        let (probability, adjustment) =
            self.adjust(top, p0, crew.as_deref(), complexity.as_deref(), shrink);
        riskiness.probability = Some(probability);
        riskiness.unadjusted_probability = Some(top);
        riskiness.support_state = Some(if uncovered_anchors > 0 || unsupported_selectors > 0 {
            SupportState::Partial
        } else {
            SupportState::Full
        });
        riskiness.driving_files = supported.into_iter().take(DRIVING_FILES).collect();
        riskiness.adjustment = Some(adjustment);
        riskiness
    }

    /// The done-task observations under `anchor`: a file's own key, or every
    /// key under a directory, each task once.
    fn anchor_observations(&self, anchor: &Anchor) -> BTreeSet<usize> {
        if !anchor.dir {
            return self
                .keys
                .get(&anchor.path)
                .into_iter()
                .flatten()
                .copied()
                .collect();
        }
        let prefix = format!("{}/", anchor.path);
        self.keys
            .range(prefix.clone()..)
            .take_while(|(key, _)| key.starts_with(&prefix))
            .flat_map(|(_, touching)| touching.iter().copied())
            .collect()
    }

    /// Apply the crew × complexity odds adjustment to `top`.
    fn adjust(
        &self,
        top: f64,
        p0: f64,
        crew: Option<&str>,
        complexity: Option<&str>,
        shrink: impl Fn(usize, usize) -> f64,
    ) -> (f64, RiskinessAdjustment) {
        let required = GROUP_MIN_SUPPORT.max(self.min_support);
        let omitted = |reason, groups| {
            (
                top,
                RiskinessAdjustment {
                    state: AdjustmentState::Omitted,
                    reason: Some(reason),
                    min_support: required,
                    groups,
                },
            )
        };
        let Some(complexity) = complexity else {
            return omitted("no_complexity", Vec::new());
        };
        let group = |crew: &str, weight: Option<u32>| {
            let tally = self
                .groups
                .get(&(crew.to_string(), complexity.to_string()))
                .copied()
                .unwrap_or_default();
            let rate = (tally.tasks >= required).then(|| shrink(tally.regressed, tally.tasks));
            RiskinessGroup {
                crew: crew.to_string(),
                weight,
                tasks: tally.tasks,
                regressed: tally.regressed,
                rate,
                applied: false,
            }
        };
        let adjusted = |rate: f64| logistic(logit(top) + logit(rate) - logit(p0));
        if self.regressed.len() < required {
            let groups = crew.map(|crew| group(crew, None)).into_iter().collect();
            return omitted("workspace_below_support", groups);
        }
        if let Some(crew) = crew {
            let mut basis = group(crew, None);
            let Some(rate) = basis.rate else {
                return omitted("group_below_support", vec![basis]);
            };
            basis.applied = true;
            return (
                adjusted(rate),
                RiskinessAdjustment {
                    state: AdjustmentState::Applied,
                    reason: None,
                    min_support: required,
                    groups: vec![basis],
                },
            );
        }
        let pool = self.pools.get(complexity).map(Vec::as_slice).unwrap_or(&[]);
        if pool.is_empty() {
            return omitted("no_crew", Vec::new());
        }
        let mut groups: Vec<RiskinessGroup> = pool
            .iter()
            .map(|(crew, weight)| group(crew, Some(*weight)))
            .collect();
        if groups.iter().all(|basis| basis.rate.is_none()) {
            return omitted("pool_below_support", groups);
        }
        let total: f64 = pool.iter().map(|(_, weight)| f64::from(*weight)).sum();
        let mut probability = 0.0;
        for basis in &mut groups {
            let weight = f64::from(basis.weight.unwrap_or_default());
            let member = match basis.rate {
                Some(rate) => {
                    basis.applied = true;
                    adjusted(rate)
                }
                None => top,
            };
            probability += weight / total * member;
        }
        let partial = groups.iter().any(|basis| !basis.applied);
        (
            probability,
            RiskinessAdjustment {
                state: AdjustmentState::PoolWeighted,
                reason: partial.then_some("pool_partial_support"),
                min_support: required,
                groups,
            },
        )
    }
}

fn logit(p: f64) -> f64 {
    (p / (1.0 - p)).ln()
}

fn logistic(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}
