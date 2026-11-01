//! `workflow.crew_ladder.*`: the operator's ordered ladder of crews, cheapest
//! first, and the knobs of the standing trial each complexity tier runs for
//! the rung below its floor [crew-ladder scope].
//!
//! The ladder is opt-in: `mode` is `off` and `tiers` is empty by default, so
//! no trial runs until both are set. Phase 1 admits `off` and `shadow` only,
//! and `delivery_quality` only `off`. The test knobs (`margin`, `look_every`,
//! `max_sample`, `watch_window`, `retry_after_days`, `quality_grade_margin`,
//! `min_grade_coverage`) are validated now so a ladder written today keeps
//! loading when they take effect.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_types::identity::{Crew, resolve_crew};
use orbit_types::task::{TaskComplexity, TaskType};

use crate::ConfigSnapshot;

/// The `workflow.crew_ladder.mode` key as written in `config.toml`.
pub const CREW_LADDER_MODE_KEY: &str = "workflow.crew_ladder.mode";
/// The `workflow.crew_ladder.rungs` key as written in `config.toml`.
pub const CREW_LADDER_RUNGS_KEY: &str = "workflow.crew_ladder.rungs";
/// The `workflow.crew_ladder.tiers` key as written in `config.toml`.
pub const CREW_LADDER_TIERS_KEY: &str = "workflow.crew_ladder.tiers";
/// The `workflow.crew_ladder.explore_rate` key as written in `config.toml`.
pub const CREW_LADDER_EXPLORE_RATE_KEY: &str = "workflow.crew_ladder.explore_rate";
/// The `workflow.crew_ladder.margin` key as written in `config.toml`.
pub const CREW_LADDER_MARGIN_KEY: &str = "workflow.crew_ladder.margin";
/// The `workflow.crew_ladder.look_every` key as written in `config.toml`.
pub const CREW_LADDER_LOOK_EVERY_KEY: &str = "workflow.crew_ladder.look_every";
/// The `workflow.crew_ladder.max_sample` key as written in `config.toml`.
pub const CREW_LADDER_MAX_SAMPLE_KEY: &str = "workflow.crew_ladder.max_sample";
/// The `workflow.crew_ladder.exclude_types` key as written in `config.toml`.
pub const CREW_LADDER_EXCLUDE_TYPES_KEY: &str = "workflow.crew_ladder.exclude_types";
/// The `workflow.crew_ladder.delivery_quality` key as written in `config.toml`.
pub const CREW_LADDER_DELIVERY_QUALITY_KEY: &str = "workflow.crew_ladder.delivery_quality";
/// The `workflow.crew_ladder.quality_grade_margin` key as written in
/// `config.toml`.
pub const CREW_LADDER_QUALITY_GRADE_MARGIN_KEY: &str = "workflow.crew_ladder.quality_grade_margin";
/// The `workflow.crew_ladder.min_grade_coverage` key as written in
/// `config.toml`.
pub const CREW_LADDER_MIN_GRADE_COVERAGE_KEY: &str = "workflow.crew_ladder.min_grade_coverage";
/// `workflow.crew_ladder.explore_rate` when unset.
pub const DEFAULT_CREW_LADDER_EXPLORE_RATE: f64 = 0.10;
/// The smallest admitted `workflow.crew_ladder.explore_rate`.
pub const MIN_CREW_LADDER_EXPLORE_RATE: f64 = 0.02;
/// The largest admitted `workflow.crew_ladder.explore_rate`.
pub const MAX_CREW_LADDER_EXPLORE_RATE: f64 = 0.25;
/// `workflow.crew_ladder.margin` when unset, in quality points.
pub const DEFAULT_CREW_LADDER_MARGIN: f64 = 5.0;
/// `workflow.crew_ladder.quality_grade_margin` when unset, in grade points.
pub const DEFAULT_CREW_LADDER_QUALITY_GRADE_MARGIN: f64 = 10.0;
/// `workflow.crew_ladder.min_grade_coverage` when unset.
pub const DEFAULT_CREW_LADDER_MIN_GRADE_COVERAGE: f64 = 0.8;
/// Arm weights are integers: a trial's two arms share this many tickets,
/// the candidate holding `round(explore_rate × scale)` of them.
pub const CREW_LADDER_WEIGHT_SCALE: u32 = 1000;

/// What the ladder does (`workflow.crew_ladder.mode`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CrewLadderMode {
    /// No trial runs and no trial is reported.
    #[default]
    Off,
    /// Each listed tier runs its trial as a shadow experiment: assignments
    /// are recorded and no task's crew changes.
    Shadow,
}

impl CrewLadderMode {
    /// Every value this release admits, in the order errors name them.
    pub const CHOICES: [&'static str; 2] = ["off", "shadow"];
    /// Values the ladder design reserves for later phases.
    const RESERVED: [&'static str; 2] = ["propose", "auto"];

    /// A mode by its configured name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "off" => Some(Self::Off),
            "shadow" => Some(Self::Shadow),
            _ => None,
        }
    }

    /// The mode's configured name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
        }
    }
}

/// Admit `workflow.crew_ladder.mode`, `off` when unset. `propose` and `auto`
/// are refused by name: they belong to later crew-ladder phases.
pub(crate) fn admit_crew_ladder_mode(raw: Option<String>) -> Result<String, OrbitError> {
    let value = raw.as_deref().map_or("off", str::trim);
    if let Some(mode) = CrewLadderMode::parse(value) {
        return Ok(mode.as_str().to_string());
    }
    if CrewLadderMode::RESERVED.contains(&value) {
        return Err(OrbitError::InvalidInput(format!(
            "{CREW_LADDER_MODE_KEY} = '{value}' is not available yet: this release runs crew-ladder \
             phase 1 (ladder report and shadow trials), so only {} are accepted",
            CrewLadderMode::CHOICES.join(" or ")
        )));
    }
    Err(OrbitError::InvalidInput(format!(
        "{CREW_LADDER_MODE_KEY} has invalid value '{value}'; expected one of: {}",
        CrewLadderMode::CHOICES.join(", ")
    )))
}

/// Admit `workflow.crew_ladder.delivery_quality`, `off` when unset. `shadow`
/// and `guardrail` are refused by name: they belong to later crew-ladder
/// phases.
pub(crate) fn admit_crew_ladder_delivery_quality(
    raw: Option<String>,
) -> Result<String, OrbitError> {
    const RESERVED: [&str; 2] = ["shadow", "guardrail"];
    let value = raw.as_deref().map_or("off", str::trim);
    if value == "off" {
        return Ok(value.to_string());
    }
    if RESERVED.contains(&value) {
        return Err(OrbitError::InvalidInput(format!(
            "{CREW_LADDER_DELIVERY_QUALITY_KEY} = '{value}' is not available yet: this release \
             runs crew-ladder phase 1 (ladder report and shadow trials), so only off is accepted"
        )));
    }
    Err(OrbitError::InvalidInput(format!(
        "{CREW_LADDER_DELIVERY_QUALITY_KEY} has invalid value '{value}'; expected one of: off, \
         shadow, guardrail"
    )))
}

/// Admit `workflow.crew_ladder.tiers`: distinct assessed complexity tiers,
/// none when unset, so the ladder is opt-in per tier.
pub(crate) fn admit_crew_ladder_tiers(raw: Option<Vec<String>>) -> Result<Vec<String>, OrbitError> {
    let raw = raw.unwrap_or_default();
    let mut seen = BTreeSet::new();
    let mut tiers = Vec::with_capacity(raw.len());
    for entry in raw {
        let tier = entry.trim();
        if !matches!(
            tier.parse::<TaskComplexity>(),
            Ok(TaskComplexity::Low
                | TaskComplexity::Medium
                | TaskComplexity::Hard
                | TaskComplexity::XHard)
        ) {
            return Err(OrbitError::InvalidInput(format!(
                "{CREW_LADDER_TIERS_KEY} names unknown tier '{tier}'; expected low, medium, hard \
                 or xhard"
            )));
        }
        if !seen.insert(tier.to_string()) {
            return Err(OrbitError::InvalidInput(format!(
                "{CREW_LADDER_TIERS_KEY} names tier '{tier}' more than once"
            )));
        }
        tiers.push(tier.to_string());
    }
    Ok(tiers)
}

/// Admit `workflow.crew_ladder.explore_rate`: the candidate arm's share,
/// between 0.02 and 0.25, `0.10` when unset.
pub(crate) fn admit_crew_ladder_explore_rate(raw: Option<f64>) -> Result<f64, OrbitError> {
    let rate = raw.unwrap_or(DEFAULT_CREW_LADDER_EXPLORE_RATE);
    if rate.is_finite()
        && (MIN_CREW_LADDER_EXPLORE_RATE..=MAX_CREW_LADDER_EXPLORE_RATE).contains(&rate)
    {
        Ok(rate)
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{CREW_LADDER_EXPLORE_RATE_KEY} must be between {MIN_CREW_LADDER_EXPLORE_RATE} and \
             {MAX_CREW_LADDER_EXPLORE_RATE}, got {rate}"
        )))
    }
}

/// Admit `workflow.crew_ladder.margin`: quality points above 0 and at most
/// 100, `5` when unset.
pub(crate) fn admit_crew_ladder_margin(raw: Option<f64>) -> Result<f64, OrbitError> {
    let margin = raw.unwrap_or(DEFAULT_CREW_LADDER_MARGIN);
    if margin.is_finite() && margin > 0.0 && margin <= 100.0 {
        Ok(margin)
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{CREW_LADDER_MARGIN_KEY} must be a number of quality points above 0 and at most \
             100, got {margin}"
        )))
    }
}

/// Admit `workflow.crew_ladder.quality_grade_margin`: grade points above 0
/// and at most 100, `10` when unset.
pub(crate) fn admit_crew_ladder_quality_grade_margin(raw: Option<f64>) -> Result<f64, OrbitError> {
    let margin = raw.unwrap_or(DEFAULT_CREW_LADDER_QUALITY_GRADE_MARGIN);
    if margin.is_finite() && margin > 0.0 && margin <= 100.0 {
        Ok(margin)
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{CREW_LADDER_QUALITY_GRADE_MARGIN_KEY} must be a number of grade points above 0 and \
             at most 100, got {margin}"
        )))
    }
}

/// Admit `workflow.crew_ladder.min_grade_coverage`: a share above 0 and at
/// most 1, `0.8` when unset.
pub(crate) fn admit_crew_ladder_min_grade_coverage(raw: Option<f64>) -> Result<f64, OrbitError> {
    let coverage = raw.unwrap_or(DEFAULT_CREW_LADDER_MIN_GRADE_COVERAGE);
    if coverage.is_finite() && coverage > 0.0 && coverage <= 1.0 {
        Ok(coverage)
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{CREW_LADDER_MIN_GRADE_COVERAGE_KEY} must be a share above 0 and at most 1, got \
             {coverage}"
        )))
    }
}

/// Admit `workflow.crew_ladder.exclude_types`: distinct task types
/// (`feature`, `bug`, `refactor`, `chore`), never all of them.
pub(crate) fn admit_crew_ladder_exclude_types(
    raw: Option<Vec<String>>,
) -> Result<Vec<String>, OrbitError> {
    let mut types = BTreeSet::new();
    for entry in raw.unwrap_or_default() {
        let name = entry.trim();
        name.parse::<TaskType>().map_err(|error| {
            OrbitError::InvalidInput(format!("{CREW_LADDER_EXCLUDE_TYPES_KEY}: {error}"))
        })?;
        types.insert(name.to_string());
    }
    if types.len() == TaskType::valid_names().len() {
        return Err(OrbitError::InvalidInput(format!(
            "{CREW_LADDER_EXCLUDE_TYPES_KEY} excludes every task type, so no task could be \
             trialled; remove one, or set {CREW_LADDER_MODE_KEY} = 'off'"
        )));
    }
    Ok(types.into_iter().collect())
}

/// Admit `workflow.crew_ladder.rungs` in place against the crew registry:
/// each rung a known, enabled crew reference (`crew` or `crew@effort`) named
/// once, rewritten to its canonical name. Order is kept: it is the operator's cheapest-first order.
pub(crate) fn admit_crew_ladder_rungs(
    rungs: &mut [String],
    crews: &BTreeMap<String, Crew>,
) -> Result<(), OrbitError> {
    let mut seen = BTreeSet::new();
    for rung in rungs.iter_mut() {
        let crew = resolve_crew(rung, crews).map_err(|error| {
            OrbitError::InvalidInput(format!("{CREW_LADDER_RUNGS_KEY}: {error}"))
        })?;
        if !crew.enabled {
            return Err(OrbitError::InvalidInput(format!(
                "{CREW_LADDER_RUNGS_KEY} names crew '{}', which is disabled ([crews.{}] enabled = \
                 false)",
                crew.name,
                crew.base_name()
            )));
        }
        if !seen.insert(crew.name.clone()) {
            return Err(OrbitError::InvalidInput(format!(
                "{CREW_LADDER_RUNGS_KEY} names crew '{}' more than once; each rung is one crew",
                crew.name
            )));
        }
        *rung = crew.name;
    }
    Ok(())
}

/// Refuse a `max_sample` smaller than `look_every`: a trial would stop
/// before its first look.
pub(crate) fn check_crew_ladder_sample(look_every: u32, max_sample: u32) -> Result<(), OrbitError> {
    if max_sample < look_every {
        return Err(OrbitError::InvalidInput(format!(
            "{CREW_LADDER_MAX_SAMPLE_KEY} ({max_sample}) must be at least \
             {CREW_LADDER_LOOK_EVERY_KEY} ({look_every})"
        )));
    }
    Ok(())
}

/// The admitted `workflow.crew_ladder.*` settings.
#[derive(Debug, Clone, PartialEq)]
pub struct CrewLadderSettings {
    /// `workflow.crew_ladder.mode`.
    pub mode: CrewLadderMode,
    /// `workflow.crew_ladder.rungs`: canonical crew names, cheapest first.
    pub rungs: Vec<String>,
    /// `workflow.crew_ladder.tiers`: the tiers that run trials, in order;
    /// empty unless the operator names them.
    pub tiers: Vec<TaskComplexity>,
    /// `workflow.crew_ladder.explore_rate`: the candidate arm's share.
    pub explore_rate: f64,
    /// `workflow.crew_ladder.margin`: non-inferiority margin, quality points.
    pub margin: f64,
    /// `workflow.crew_ladder.look_every`: settled candidate tasks per look.
    pub look_every: u32,
    /// `workflow.crew_ladder.max_sample`: candidate tasks before a stop.
    pub max_sample: u32,
    /// `workflow.crew_ladder.exclude_types`: task types never trialled.
    pub exclude_types: Vec<TaskType>,
    /// `workflow.crew_ladder.watch_window`: settled tasks per demotion check.
    pub watch_window: u32,
    /// `workflow.crew_ladder.retry_after_days`: days before a failed rung
    /// is trialled again.
    pub retry_after_days: u32,
    /// `workflow.crew_ladder.quality_grade_margin`: grade points a candidate
    /// may trail by under a delivery-quality check (later phases).
    pub quality_grade_margin: f64,
    /// `workflow.crew_ladder.min_grade_coverage`: the share of settled tasks
    /// that must carry a grade before a delivery-quality check reads them
    /// (later phases).
    pub min_grade_coverage: f64,
}

impl CrewLadderSettings {
    /// The `(control, candidate)` arm weights: `explore_rate` of
    /// [`CREW_LADDER_WEIGHT_SCALE`] tickets, rounded, to the candidate.
    #[must_use]
    pub fn arm_weights(&self) -> (u32, u32) {
        let scale = f64::from(CREW_LADDER_WEIGHT_SCALE);
        // explore_rate is admitted in [0.02, 0.25], so this is 20..=250.
        let candidate = (self.explore_rate * scale).round().clamp(1.0, scale - 1.0);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the rounded weight is clamped from 1 to one below the weight scale, and that integer converts to u32 exactly"
        )]
        let candidate = candidate as u32;
        (CREW_LADDER_WEIGHT_SCALE - candidate, candidate)
    }
}

impl ConfigSnapshot {
    /// The admitted `workflow.crew_ladder.*` settings.
    #[must_use]
    pub fn crew_ladder(&self) -> CrewLadderSettings {
        CrewLadderSettings {
            mode: CrewLadderMode::parse(&self.workflow_crew_ladder_mode).unwrap_or_default(),
            rungs: self.workflow_crew_ladder_rungs.clone(),
            tiers: self
                .workflow_crew_ladder_tiers
                .iter()
                .filter_map(|tier| tier.parse().ok())
                .collect(),
            explore_rate: self.workflow_crew_ladder_explore_rate,
            margin: self.workflow_crew_ladder_margin,
            look_every: self.workflow_crew_ladder_look_every,
            max_sample: self.workflow_crew_ladder_max_sample,
            exclude_types: self
                .workflow_crew_ladder_exclude_types
                .iter()
                .filter_map(|name| name.parse().ok())
                .collect(),
            watch_window: self.workflow_crew_ladder_watch_window,
            retry_after_days: self.workflow_crew_ladder_retry_after_days,
            quality_grade_margin: self.workflow_crew_ladder_quality_grade_margin,
            min_grade_coverage: self.workflow_crew_ladder_min_grade_coverage,
        }
    }
}
