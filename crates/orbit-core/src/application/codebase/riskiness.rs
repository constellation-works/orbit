//! Per-task riskiness: the observed chance that a task, once done, is named
//! by at least one `regression_from` link, estimated from its size alone.
//!
//! The estimate reads the same windowed regression facts as the
//! regression-risk marks ([`super::risk`]) and the Regressions ranking: every
//! task done in the trailing `workflow.regression_risk.window_days` is one
//! observation, regressed when any link names it. The marks' threshold and
//! file cap do not apply.
//!
//! 1. A task's size has one of two bases. Before a diff exists it is the
//!    declared path count: the distinct `file:`/`symbol:` paths and `dir:`
//!    directories of its `context_files`, each counted once; module and
//!    command selectors carry no path and are counted as unsupported. Once
//!    the task is in review or done and its diff resolves, the size is the
//!    lines changed (insertions plus deletions) of that diff: a done task's
//!    landed-delivery row ([`super::diff_stats`], written once per landing),
//!    or a review task's candidate commit against the base its delivery run
//!    started from, diffed once per commit pair per process. A review or
//!    done task without a resolvable diff falls back to declared files and
//!    says so.
//! 2. Each basis has its own fit of
//!    `p = 1 - exp(-exp(a + b·ln(1 + size)))`, with `a` and `b` refitted by
//!    maximum likelihood (Fisher scoring) at every model build over the
//!    window's done tasks that have that size. When every observation has
//!    the same size the slope is not identified and the fit is the
//!    intercept alone (`b = 0`).
//! 3. A fit needs at least `min_support` observations, some regressed and
//!    some not, and a finite optimum. Otherwise the state names why
//!    (`no_footprint`, `unsupported_footprint` or `insufficient_history`),
//!    never a low probability.
//! 4. Every estimate carries its calibration evidence: of the window's done
//!    tasks on the same basis at least this size, how many regressed.
//!
//! Size carries the signal: in backtests of Orbit's own history, neither a
//! crew × complexity term nor per-file attribution added anything over it,
//! so the estimate names neither. The estimate is derived, not stored on the
//! task; [`TaskRiskinessModel`] is built once per scan and estimates any
//! number of tasks without another history read. The backlog forecast
//! consumes [`TaskRiskiness::probability`] unchanged.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::LandedDiffStats;
use orbit_types::task::{Task, TaskStatus};
use serde::Serialize;
use serde_json::Value;

use super::facts::{Anchor, CodebaseFacts};
use super::regression::RegressionFilter;
use super::sections::{Params, Rollup};
use crate::OrbitRuntime;
use crate::application::job::CodebaseDeliveryScan;

/// Revision of the estimator below. Bump it whenever a change alters an
/// estimate for the same facts. 2: size-based fit in place of the per-file
/// maximum and the crew × complexity adjustment.
pub const TASK_RISKINESS_ESTIMATOR_VERSION: u32 = 2;
/// Fisher-scoring iterations one fit may take before it is abandoned.
const FIT_MAX_ITERATIONS: usize = 100;
/// A fit has converged once a full step moves no parameter further.
const FIT_TOLERANCE: f64 = 1e-9;
/// A parameter past this magnitude means the sizes separate regressed from
/// clean tasks: the likelihood has no finite optimum.
const FIT_MAX_PARAMETER: f64 = 50.0;
/// Review-candidate diffs one process remembers before starting over.
const CANDIDATE_CACHE_LIMIT: usize = 4_096;

/// Whether a task has an estimate, and why not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskinessState {
    /// A probability from a fitted size model.
    Estimated,
    /// The task declares no `context_files` and has no resolvable diff.
    NoFootprint,
    /// The task's selectors name no path (module or command selectors only).
    UnsupportedFootprint,
    /// The window holds too few done tasks on the task's basis to fit.
    InsufficientHistory,
}

/// How much of an estimated task's size was measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SupportState {
    /// The whole size was measured.
    Full,
    /// Some selectors name no path, so the declared count is a lower bound.
    Partial,
}

/// What a task's size counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskinessBasis {
    /// Distinct paths the task's `context_files` declare.
    DeclaredFiles,
    /// Lines inserted plus deleted by the task's candidate or landed diff.
    DiffLines,
}

/// Why a task in review or done is sized by declared files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BasisFallback {
    /// No candidate or landed diff of the task could be read.
    NoResolvableDiff,
    /// The diff resolved, but too few done tasks in the window have one to
    /// fit lines changed.
    DiffHistoryInsufficient,
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

/// One basis's fitted `p = 1 - exp(-exp(intercept + slope·ln(1 + size)))`.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct RiskinessFit {
    /// Done tasks in the window with a size on this basis.
    pub observations: usize,
    /// Of them, regressed.
    pub regressed: usize,
    pub intercept: f64,
    pub slope: f64,
}

/// The calibration evidence of one estimate: done tasks in the window, on
/// the same basis, at least as large as the task.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct RiskinessSizeBand {
    pub tasks: usize,
    pub regressed: usize,
    /// `regressed / tasks`; `None` when no task was that large.
    pub rate: Option<f64>,
}

/// One task's riskiness, as the task API serves it.
#[derive(Debug, Clone, Serialize)]
pub struct TaskRiskiness {
    pub estimator_version: u32,
    pub state: RiskinessState,
    /// The fitted estimate; `None` unless `state` is `estimated`.
    pub probability: Option<f64>,
    /// What `size` counts; `None` when the task has no size.
    pub basis: Option<RiskinessBasis>,
    /// Why a task in review or done is sized by declared files.
    pub basis_fallback: Option<BasisFallback>,
    /// Declared paths or lines changed, per `basis`.
    pub size: Option<u64>,
    /// The fit behind `probability`, when one exists for the basis.
    pub fit: Option<RiskinessFit>,
    /// Done tasks in the window at least this size, and how many regressed.
    pub size_band: Option<RiskinessSizeBand>,
    pub support_state: Option<SupportState>,
    pub window_days: u32,
    /// The fewest observations a fit needs.
    pub min_support: usize,
    /// Start of the window.
    pub since: String,
    /// When the facts were read: later regressions are not yet counted, and
    /// tasks done shortly before it have had little time to reveal any.
    pub observed_through: String,
    pub workspace: RiskinessBase,
    /// Done tasks behind the fit; equal to `fit.observations`.
    pub supporting_tasks: usize,
    /// Of them, regressed.
    pub supporting_regressed: usize,
    /// Distinct path anchors the footprint names.
    pub anchors: usize,
    /// Selectors that name no path.
    pub unsupported_selectors: usize,
    /// Deprecated in estimator version 2: always null.
    pub unadjusted_probability: Option<f64>,
    /// Deprecated in estimator version 2: always 0.
    pub uncovered_anchors: usize,
    /// Deprecated in estimator version 2: always empty.
    pub driving_files: Vec<Value>,
    /// Deprecated in estimator version 2: always null.
    pub crew: Option<String>,
    /// Deprecated in estimator version 2: always null.
    pub complexity: Option<String>,
    /// Deprecated in estimator version 2: always null.
    pub adjustment: Option<Value>,
}

/// Done tasks in one crew × complexity group.
#[derive(Debug, Default, Clone, Copy)]
struct GroupTally {
    tasks: usize,
    regressed: usize,
}

/// The window's observations on one basis, sorted by size for the size band,
/// and their fit.
#[derive(Debug, Default)]
struct SizeHistory {
    /// Sizes, ascending.
    sizes: Vec<u64>,
    /// `at_least[i]`: regressed observations among `sizes[i..]`.
    regressed_at_least: Vec<usize>,
    fit: Option<RiskinessFit>,
}

impl SizeHistory {
    fn new(mut points: Vec<(u64, bool)>, min_support: usize) -> Self {
        let fit = fit_size_model(&points, min_support);
        points.sort_unstable();
        let mut regressed_at_least = vec![0; points.len() + 1];
        for (index, (_, regressed)) in points.iter().enumerate().rev() {
            regressed_at_least[index] = regressed_at_least[index + 1] + usize::from(*regressed);
        }
        Self {
            sizes: points.into_iter().map(|(size, _)| size).collect(),
            regressed_at_least,
            fit,
        }
    }

    fn band(&self, size: u64) -> RiskinessSizeBand {
        let start = self.sizes.partition_point(|observed| *observed < size);
        let tasks = self.sizes.len() - start;
        let regressed = self.regressed_at_least[start];
        RiskinessSizeBand {
            tasks,
            regressed,
            rate: (tasks > 0).then(|| regressed as f64 / tasks as f64),
        }
    }
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
    /// Observations by (crew, complexity), for the forecast prior only.
    groups: BTreeMap<(String, String), GroupTally>,
    declared: SizeHistory,
    diff: SizeHistory,
    /// Lines changed by each task's landed delivery or review candidate.
    diff_lines: HashMap<String, u64>,
}

impl OrbitRuntime {
    /// Read the windowed regression facts and the recorded diff sizes once
    /// and build the model that estimates every task's riskiness from them,
    /// under the configured `workflow.regression_risk` window and minimum
    /// support.
    pub fn task_riskiness_model(&self) -> Result<TaskRiskinessModel, OrbitError> {
        let settings = self.regression_risk_settings();
        let since = Utc::now() - Duration::days(i64::from(settings.window_days));
        let facts = super::facts::scan_regressions(self, since)?;
        let mut diff_lines: HashMap<String, u64> = self
            .landed_diff_stats(None)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "riskiness: landed diff stats unreadable");
                Vec::new()
            })
            .iter()
            // Oldest first: a task's latest landing wins.
            .map(|row| (row.task_id.clone(), landed_lines(row)))
            .collect();
        self.add_review_candidate_lines(&facts, &mut diff_lines);
        Ok(TaskRiskinessModel::build(
            &facts,
            since,
            settings.window_days,
            settings.min_support as usize,
            diff_lines,
        ))
    }

    /// The lines changed by each review task's candidate, from the commit
    /// its delivery run made against the base it started from.
    fn add_review_candidate_lines(
        &self,
        facts: &CodebaseFacts,
        diff_lines: &mut HashMap<String, u64>,
    ) {
        let scan = CodebaseDeliveryScan::default();
        let mut repo: Option<Option<git2::Repository>> = None;
        for task in &facts.tasks {
            if task.status != TaskStatus::Review || diff_lines.contains_key(&task.id) {
                continue;
            }
            let Ok(delivery) = self.observe_codebase_task_delivery(&task.id, &scan) else {
                continue;
            };
            let (Some(base), Some(head)) = (delivery.commit.base_sha, delivery.commit.head_sha)
            else {
                continue;
            };
            let key = (base, head);
            let cache = candidate_lines_cache();
            if let Some(lines) = cache.lock().ok().and_then(|seen| seen.get(&key).copied()) {
                diff_lines.insert(task.id.clone(), lines);
                continue;
            }
            let repo = repo
                .get_or_insert_with(|| git2::Repository::open(&self.paths().repo_root).ok())
                .as_ref();
            // A missing commit is not remembered: a later fetch can supply it.
            let Some(lines) = repo.and_then(|repo| self.candidate_diff_lines(repo, &key.0, &key.1))
            else {
                continue;
            };
            if let Ok(mut seen) = cache.lock() {
                if seen.len() >= CANDIDATE_CACHE_LIMIT {
                    seen.clear();
                }
                seen.insert(key, lines);
            }
            diff_lines.insert(task.id.clone(), lines);
        }
    }
}

/// Lines changed per candidate `(base, head)`. A commit pair's diff never
/// changes, so the process diffs each one once.
fn candidate_lines_cache() -> &'static Mutex<HashMap<(String, String), u64>> {
    static CANDIDATE_LINES: OnceLock<Mutex<HashMap<(String, String), u64>>> = OnceLock::new();
    CANDIDATE_LINES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn landed_lines(row: &LandedDiffStats) -> u64 {
    [row.code, row.tests, row.other]
        .iter()
        .map(|class| class.added + class.deleted)
        .sum()
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
        diff_lines: HashMap<String, u64>,
    ) -> Self {
        let params = Params {
            since: Some(since),
            min_support: min_support.max(1),
            rollup: Rollup::File,
            filter: RegressionFilter::default(),
        };
        let mut regressed = Vec::new();
        let mut groups: BTreeMap<(String, String), GroupTally> = BTreeMap::new();
        let mut declared = Vec::new();
        let mut diff = Vec::new();
        for task in &facts.tasks {
            if !task.done_at.is_some_and(|at| params.within(at)) {
                continue;
            }
            let was_regressed = params.filter.regressed(task);
            regressed.push(was_regressed);
            declared.push((task.footprint.len() as u64, was_regressed));
            if let Some(lines) = diff_lines.get(&task.id) {
                diff.push((*lines, was_regressed));
            }
            if let (Some(crew), Some(complexity)) = (&task.crew, &task.complexity) {
                let tally = groups
                    .entry((crew.clone(), complexity.clone()))
                    .or_default();
                tally.tasks += 1;
                tally.regressed += usize::from(was_regressed);
            }
        }
        Self {
            computed_at: facts.computed_at,
            since,
            window_days,
            min_support: params.min_support,
            regressed,
            groups,
            declared: SizeHistory::new(declared, params.min_support),
            diff: SizeHistory::new(diff, params.min_support),
            diff_lines,
        }
    }

    /// The riskiness of `task` from this model's facts.
    pub fn estimate(&self, task: &Task) -> TaskRiskiness {
        let population = self.regressed.len();
        let regressed = self.regressed.iter().filter(|flag| **flag).count();
        let p0 = (regressed as f64 + 1.0) / (population as f64 + 2.0);

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

        let diff = matches!(task.status, TaskStatus::Review | TaskStatus::Done)
            .then(|| self.diff_lines.get(task.id.as_str()).copied());
        let (basis, size, basis_fallback) = match diff {
            Some(Some(lines)) if self.diff.fit.is_some() => {
                (RiskinessBasis::DiffLines, lines, None)
            }
            Some(Some(_)) => (
                RiskinessBasis::DeclaredFiles,
                anchors.len() as u64,
                Some(BasisFallback::DiffHistoryInsufficient),
            ),
            Some(None) => (
                RiskinessBasis::DeclaredFiles,
                anchors.len() as u64,
                Some(BasisFallback::NoResolvableDiff),
            ),
            None => (RiskinessBasis::DeclaredFiles, anchors.len() as u64, None),
        };
        let history = match basis {
            RiskinessBasis::DeclaredFiles => &self.declared,
            RiskinessBasis::DiffLines => &self.diff,
        };
        let state = if basis == RiskinessBasis::DiffLines {
            RiskinessState::Estimated
        } else if selectors.is_empty() {
            RiskinessState::NoFootprint
        } else if anchors.is_empty() {
            RiskinessState::UnsupportedFootprint
        } else if history.fit.is_none() {
            RiskinessState::InsufficientHistory
        } else {
            RiskinessState::Estimated
        };
        let sized = matches!(
            state,
            RiskinessState::Estimated | RiskinessState::InsufficientHistory
        );
        let fit = history.fit.filter(|_| sized);
        let estimated = state == RiskinessState::Estimated;
        TaskRiskiness {
            estimator_version: TASK_RISKINESS_ESTIMATOR_VERSION,
            state,
            probability: fit.filter(|_| estimated).map(|fit| fit.probability(size)),
            basis: sized.then_some(basis),
            basis_fallback: basis_fallback.filter(|_| sized),
            size: sized.then_some(size),
            fit,
            size_band: estimated.then(|| history.band(size)),
            support_state: estimated.then(|| {
                if basis == RiskinessBasis::DeclaredFiles && unsupported_selectors > 0 {
                    SupportState::Partial
                } else {
                    SupportState::Full
                }
            }),
            window_days: self.window_days,
            min_support: self.min_support,
            since: self.since.to_rfc3339_opts(SecondsFormat::Secs, true),
            observed_through: self.computed_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            workspace: RiskinessBase {
                tasks: population,
                regressed,
                base_rate: p0,
            },
            supporting_tasks: fit.map_or(0, |fit| fit.observations),
            supporting_regressed: fit.map_or(0, |fit| fit.regressed),
            anchors: anchors.len(),
            unsupported_selectors,
            unadjusted_probability: None,
            uncovered_anchors: 0,
            driving_files: Vec::new(),
            crew: None,
            complexity: None,
            adjustment: None,
        }
    }
}

impl RiskinessFit {
    fn probability(&self, size: u64) -> f64 {
        let hazard = (self.intercept + self.slope * size_covariate(size)).exp();
        -(-hazard).exp_m1()
    }
}

fn size_covariate(size: u64) -> f64 {
    (size as f64).ln_1p()
}

/// The log-likelihood, score and Fisher information of one observation set
/// at `(a, b)` under the complementary log-log link.
struct FitStep {
    log_likelihood: f64,
    score: [f64; 2],
    information: [f64; 3],
}

fn fit_step(points: &[(f64, bool)], a: f64, b: f64) -> FitStep {
    let mut step = FitStep {
        log_likelihood: 0.0,
        score: [0.0; 2],
        information: [0.0; 3],
    };
    for &(x, regressed) in points {
        let hazard = (a + b * x).clamp(-40.0, 40.0).exp();
        let survival = (-hazard).exp();
        let p = (-(-hazard).exp_m1()).max(f64::MIN_POSITIVE);
        let y = if regressed { 1.0 } else { 0.0 };
        step.log_likelihood += if regressed { p.ln() } else { -hazard };
        let score = (y - p) * hazard / p;
        let weight = hazard * hazard * survival / p;
        step.score[0] += score;
        step.score[1] += score * x;
        step.information[0] += weight;
        step.information[1] += weight * x;
        step.information[2] += weight * x * x;
    }
    step
}

/// Fit `p = 1 - exp(-exp(a + b·ln(1 + size)))` by maximum likelihood.
/// `None` below `min_support` observations, without both outcomes, or when
/// the optimum is not finite.
fn fit_size_model(points: &[(u64, bool)], min_support: usize) -> Option<RiskinessFit> {
    let observations = points.len();
    let regressed = points.iter().filter(|(_, flag)| *flag).count();
    if observations < min_support.max(1) || regressed == 0 || regressed == observations {
        return None;
    }
    let rate = regressed as f64 / observations as f64;
    let intercept_only = (-(-rate).ln_1p()).ln();
    let fit = |intercept: f64, slope: f64| RiskinessFit {
        observations,
        regressed,
        intercept,
        slope,
    };
    let first = points.first().map(|(size, _)| *size);
    if points.iter().all(|(size, _)| Some(*size) == first) {
        return Some(fit(intercept_only, 0.0));
    }
    let points: Vec<(f64, bool)> = points
        .iter()
        .map(|(size, flag)| (size_covariate(*size), *flag))
        .collect();
    let (mut a, mut b) = (intercept_only, 0.0);
    let mut current = fit_step(&points, a, b);
    for _ in 0..FIT_MAX_ITERATIONS {
        let [i_aa, i_ab, i_bb] = current.information;
        let determinant = i_aa * i_bb - i_ab * i_ab;
        if !determinant.is_finite() || determinant <= f64::EPSILON * i_aa * i_bb {
            return None;
        }
        let [s_a, s_b] = current.score;
        let delta_a = (i_bb * s_a - i_ab * s_b) / determinant;
        let delta_b = (i_aa * s_b - i_ab * s_a) / determinant;
        let mut length = 1.0;
        let mut next = fit_step(&points, a + delta_a, b + delta_b);
        while next.log_likelihood < current.log_likelihood && length > 1e-9 {
            length /= 2.0;
            next = fit_step(&points, a + length * delta_a, b + length * delta_b);
        }
        a += length * delta_a;
        b += length * delta_b;
        current = next;
        if !(a.is_finite() && b.is_finite())
            || a.abs() > FIT_MAX_PARAMETER
            || b.abs() > FIT_MAX_PARAMETER
        {
            return None;
        }
        if (length * delta_a).abs().max((length * delta_b).abs()) < FIT_TOLERANCE {
            return Some(fit(a, b));
        }
    }
    None
}
