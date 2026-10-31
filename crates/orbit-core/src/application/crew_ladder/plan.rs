//! The ladder plan: per complexity tier, its configured pool, floor and
//! candidate, the stored trials, and what the trial sync should do. The
//! report renders this plan and the sync acts on it, so the two never
//! disagree about why a tier has no trial.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_config::{CrewLadderMode, CrewLadderSettings, CrewPoolEntry, canonical_crew_pool};
use orbit_runtime::CrewLadderControls;
use orbit_types::task::{TaskComplexity, TaskType};
use serde_json::{Value, json};

use super::CREW_LADDER_ACTOR;
use crate::OrbitRuntime;
use crate::application::experiment::ExperimentView;

/// Every assessed tier, in the order the report lists them.
const TIERS: [TaskComplexity; 4] = [
    TaskComplexity::Low,
    TaskComplexity::Medium,
    TaskComplexity::Hard,
    TaskComplexity::XHard,
];
/// The trial arm that keeps the tier's own pool.
pub(super) const CONTROL_ARM: &str = "control";
/// The trial arm whose crew overlay is the one candidate rung.
pub(super) const CANDIDATE_ARM: &str = "candidate";

/// One ladder trial, read back from its stored experiment.
#[derive(Debug, Clone)]
pub(super) struct LadderTrial {
    pub(super) view: ExperimentView,
    pub(super) tier: TaskComplexity,
    pub(super) candidate: String,
}

impl LadderTrial {
    /// The trial `view` is, when `system:crew-ladder` created it: its tier
    /// is the population's one complexity, its rung the candidate arm's
    /// one crew.
    pub(super) fn from_view(view: &ExperimentView) -> Option<Self> {
        if view.created_by != CREW_LADDER_ACTOR {
            return None;
        }
        let tier = match view.population["complexity"].as_array()?.as_slice() {
            [tier] => tier.as_str()?.parse().ok()?,
            _ => return None,
        };
        let candidate = view
            .arms
            .iter()
            .find(|arm| arm.name == CANDIDATE_ARM)?
            .overlay["crew"]["pool"][0]["crew"]
            .as_str()?
            .to_string();
        Some(Self {
            view: view.clone(),
            tier,
            candidate,
        })
    }

    /// Draft, running or paused: not yet concluded or abandoned.
    pub(super) fn is_open(&self) -> bool {
        matches!(self.view.status.as_str(), "draft" | "running" | "paused")
    }

    /// Started and not yet concluded or abandoned.
    fn is_started(&self) -> bool {
        matches!(self.view.status.as_str(), "running" | "paused")
    }

    /// The arm weights of the active segment, by arm.
    pub(super) fn active_weights(&self) -> BTreeMap<String, u32> {
        self.view
            .arms
            .iter()
            .filter(|arm| arm.retired_segment.is_none())
            .map(|arm| (arm.name.clone(), arm.weight))
            .collect()
    }

    /// Whether the trial is the one `settings` define for `candidate`: the
    /// same rung, arm weights and task types. A changed `explore_rate` or
    /// `exclude_types` retires the trial like a changed candidate.
    pub(super) fn matches_ladder(
        &self,
        candidate: Option<&String>,
        settings: &CrewLadderSettings,
    ) -> bool {
        Some(&self.candidate) == candidate
            && self.active_weights() == ladder_arm_weights(settings)
            && task_types(&self.view.population)
                == task_types(&ladder_trial_population(self.tier, settings))
    }
}

/// The arm weights `settings` give a trial, by arm.
pub(super) fn ladder_arm_weights(settings: &CrewLadderSettings) -> BTreeMap<String, u32> {
    let (control, candidate) = settings.arm_weights();
    BTreeMap::from([
        (CONTROL_ARM.to_string(), control),
        (CANDIDATE_ARM.to_string(), candidate),
    ])
}

/// A trial's population: the tier's tasks, minus `exclude_types` when it
/// names any.
pub(super) fn ladder_trial_population(
    tier: TaskComplexity,
    settings: &CrewLadderSettings,
) -> Value {
    let mut population = json!({ "complexity": [tier.as_str()] });
    if !settings.exclude_types.is_empty() {
        population["task_type"] = Value::from(
            TaskType::valid_names()
                .iter()
                .filter(|name| {
                    !settings
                        .exclude_types
                        .iter()
                        .any(|excluded| excluded.to_string() == **name)
                })
                .map(|name| Value::from(*name))
                .collect::<Vec<_>>(),
        );
    }
    population
}

/// The task types a population admits; empty when it names none (all).
fn task_types(population: &Value) -> BTreeSet<String> {
    population["task_type"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// Why a tier runs no trial for its candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NoTrial {
    /// `workflow.crew_ladder.mode` is `off`.
    ModeOff,
    /// The tier is not in `workflow.crew_ladder.tiers`.
    TierNotListed,
    /// No ladder rung is an enabled member of the tier's pool.
    NoLadderRung,
    /// The floor is the bottom rung, so nothing is cheaper.
    FloorIsBottomRung,
    /// A running operator crew experiment covers the tier: its id and name.
    OperatorExperiment { id: String, name: String },
    /// `experiments.enabled` is false.
    ExperimentsDisabled,
    /// No `experiments.max_running` slot is free.
    MaxRunning,
}

impl NoTrial {
    /// The reason's stable code in the report.
    pub(super) fn code(&self) -> &'static str {
        match self {
            Self::ModeOff => "mode_off",
            Self::TierNotListed => "tier_not_listed",
            Self::NoLadderRung => "no_ladder_rung",
            Self::FloorIsBottomRung => "floor_is_bottom_rung",
            Self::OperatorExperiment { .. } => "operator_experiment",
            Self::ExperimentsDisabled => "experiments_disabled",
            Self::MaxRunning => "max_running",
        }
    }
}

/// What the sync does for one tier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TierAction {
    /// The started trial with this experiment id already trials the
    /// candidate as the ladder defines it.
    Keep(String),
    /// Start a trial for the candidate, reusing a matching draft.
    Start,
    /// Run no trial.
    Skip(NoTrial),
}

/// One tier's plan.
#[derive(Debug, Clone)]
pub(super) struct TierPlan {
    pub(super) tier: TaskComplexity,
    /// Whether `workflow.crew_ladder.tiers` lists the tier.
    pub(super) listed: bool,
    /// `workflow.<tier>_complexity_crews`.
    pub(super) pool_key: String,
    /// The configured pool, canonical.
    pub(super) pool: Vec<CrewPoolEntry>,
    /// The cheapest rung among the pool's enabled, weighted members.
    pub(super) floor: Option<String>,
    /// The rung directly below the floor.
    pub(super) candidate: Option<String>,
    /// Open ladder trials of this tier, oldest first.
    pub(super) open_trials: Vec<LadderTrial>,
    pub(super) action: TierAction,
}

impl TierPlan {
    /// The draft a start reuses: the oldest open draft that matches the
    /// ladder.
    pub(super) fn reusable_draft(&self, settings: &CrewLadderSettings) -> Option<&LadderTrial> {
        if self.action != TierAction::Start {
            return None;
        }
        self.open_trials.iter().find(|trial| {
            trial.view.status == "draft" && trial.matches_ladder(self.candidate.as_ref(), settings)
        })
    }

    /// Open trials the sync concludes (started) or abandons (drafts): all
    /// but the kept trial and the reused draft.
    pub(super) fn retired<'a>(
        &'a self,
        settings: &'a CrewLadderSettings,
    ) -> impl Iterator<Item = &'a LadderTrial> {
        let kept = match &self.action {
            TierAction::Keep(id) => Some(id.as_str()),
            TierAction::Start => self
                .reusable_draft(settings)
                .map(|trial| trial.view.id.as_str()),
            TierAction::Skip(_) => None,
        };
        self.open_trials
            .iter()
            .filter(move |trial| Some(trial.view.id.as_str()) != kept)
    }
}

/// The whole ladder plan.
#[derive(Debug, Clone)]
pub(super) struct LadderPlan {
    pub(super) settings: CrewLadderSettings,
    /// `experiments.enabled`, read from the current config.
    pub(super) experiments_enabled: bool,
    /// `experiments.max_running`.
    pub(super) max_running: u32,
    /// Experiments running now, ladder trials included.
    pub(super) running: usize,
    /// One plan per assessed tier, low to xhard.
    pub(super) tiers: Vec<TierPlan>,
}

impl OrbitRuntime {
    /// Plan every tier against the workspace's experiments `views`, reading
    /// the ladder, crews, pools and experiment gates from one `controls`
    /// version so the plan never mixes config versions.
    pub(super) fn crew_ladder_plan(
        &self,
        views: &[ExperimentView],
        controls: &CrewLadderControls,
    ) -> Result<LadderPlan, OrbitError> {
        let settings = controls.settings.clone();
        let crews = &controls.crews;
        let trials = views
            .iter()
            .filter_map(LadderTrial::from_view)
            .filter(LadderTrial::is_open)
            .collect::<Vec<_>>();
        let mut tiers = Vec::with_capacity(TIERS.len());
        for tier in TIERS {
            let pool_key = format!("workflow.{tier}_complexity_crews");
            let names = controls
                .complexity_crews
                .pool(tier)
                .unwrap_or_default()
                .to_vec();
            let pool = canonical_crew_pool(&names, crews, &pool_key)?.entries;
            let members = pool
                .iter()
                .filter(|entry| {
                    entry.weight > 0
                        && orbit_types::identity::find_crew(&entry.name, crews)
                            .is_some_and(|crew| crew.enabled)
                })
                .map(|entry| entry.name.as_str())
                .collect::<BTreeSet<_>>();
            let floor_index = settings
                .rungs
                .iter()
                .position(|rung| members.contains(rung.as_str()));
            let floor = floor_index.map(|index| settings.rungs[index].clone());
            let candidate = floor_index
                .and_then(|index| index.checked_sub(1))
                .map(|index| settings.rungs[index].clone());
            let listed = settings.tiers.contains(&tier);
            let open_trials = trials
                .iter()
                .filter(|trial| trial.tier == tier)
                .cloned()
                .collect::<Vec<_>>();
            let skip = if settings.mode == CrewLadderMode::Off {
                Some(NoTrial::ModeOff)
            } else if !listed {
                Some(NoTrial::TierNotListed)
            } else if floor.is_none() {
                Some(NoTrial::NoLadderRung)
            } else if candidate.is_none() {
                Some(NoTrial::FloorIsBottomRung)
            } else {
                operator_crew_experiment(views, tier)
                    .map(|(id, name)| NoTrial::OperatorExperiment { id, name })
            };
            let action = match skip {
                Some(reason) => TierAction::Skip(reason),
                None => open_trials
                    .iter()
                    .find(|trial| {
                        trial.is_started() && trial.matches_ladder(candidate.as_ref(), &settings)
                    })
                    .map_or(TierAction::Start, |trial| {
                        TierAction::Keep(trial.view.id.clone())
                    }),
            };
            tiers.push(TierPlan {
                tier,
                listed,
                pool_key,
                pool,
                floor,
                candidate,
                open_trials,
                action,
            });
        }

        // Trials count toward `experiments.max_running`. The sync concludes
        // retired trials before it starts new ones, so their slots are free.
        let experiments_enabled = controls.experiments_enabled;
        let max_running = controls.max_running;
        let running = views.iter().filter(|view| view.status == "running").count();
        let retiring = tiers
            .iter()
            .flat_map(|tier| tier.retired(&settings))
            .filter(|trial| trial.view.status == "running")
            .count();
        let mut free = usize::try_from(max_running)
            .unwrap_or(usize::MAX)
            .saturating_sub(running.saturating_sub(retiring));
        // Slots go to tiers in `workflow.crew_ladder.tiers` order.
        for tier in &settings.tiers {
            let Some(plan) = tiers.iter_mut().find(|plan| plan.tier == *tier) else {
                continue;
            };
            if plan.action != TierAction::Start {
                continue;
            }
            if !experiments_enabled {
                plan.action = TierAction::Skip(NoTrial::ExperimentsDisabled);
            } else if free == 0 {
                plan.action = TierAction::Skip(NoTrial::MaxRunning);
            } else {
                free -= 1;
            }
        }
        Ok(LadderPlan {
            settings,
            experiments_enabled,
            max_running,
            running,
            tiers,
        })
    }
}

/// The running operator experiment, if any, whose arms overlay the crew
/// surface for `tier`'s tasks: a population naming the tier or no
/// complexity at all.
fn operator_crew_experiment(
    views: &[ExperimentView],
    tier: TaskComplexity,
) -> Option<(String, String)> {
    views
        .iter()
        .filter(|view| view.status == "running" && view.created_by != CREW_LADDER_ACTOR)
        .filter(|view| {
            view.arms
                .iter()
                .any(|arm| arm.overlay.get("crew").is_some_and(|crew| !crew.is_null()))
        })
        .find(|view| match view.population["complexity"].as_array() {
            Some(tiers) if !tiers.is_empty() => tiers
                .iter()
                .any(|name| name.as_str() == Some(tier.as_str())),
            _ => true,
        })
        .map(|view| (view.id.clone(), view.name.clone()))
}
