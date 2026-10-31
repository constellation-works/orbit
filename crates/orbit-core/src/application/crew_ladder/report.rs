//! The read-only ladder report behind `orbit crew ladder` and
//! `orbit.crew.ladder`: per tier, the configured pool, the floor, the
//! candidate, and the shadow trial with its would-be assignments, or the
//! reason there is none. It reads the plan the trial sync acts on and
//! writes nothing.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_config::{CrewLadderSettings, CrewPoolEntry};
use orbit_store::contracts::ExperimentStoreBackend as _;
use serde::Serialize;

use super::plan::{CANDIDATE_ARM, CONTROL_ARM, NoTrial, TierAction, TierPlan, ladder_arm_weights};
use crate::OrbitRuntime;

/// The whole ladder.
#[derive(Debug, Clone, Serialize)]
pub struct CrewLadderReport {
    /// Completed invocations in the last 90 days, by actual crew/model/effort.
    pub effort_breakdown: Vec<orbit_types::telemetry::AgentEffortUsage>,
    /// Historical invocation population covered by the breakdown.
    pub effort_window_days: u32,
    /// `workflow.crew_ladder.mode`.
    pub mode: String,
    /// `workflow.crew_ladder.rungs`, cheapest first.
    pub rungs: Vec<String>,
    /// `workflow.crew_ladder.tiers`.
    pub tiers: Vec<String>,
    /// `workflow.crew_ladder.explore_rate`.
    pub explore_rate: f64,
    /// The arm weights a new trial gets, by arm.
    pub arm_weights: BTreeMap<String, u32>,
    /// `experiments.enabled`, from the current config.
    pub experiments_enabled: bool,
    /// `experiments.max_running`, which trials count toward.
    pub max_running: u32,
    /// Experiments running now, ladder trials included.
    pub running: usize,
    /// One row per assessed tier, low to xhard.
    pub rows: Vec<CrewLadderRow>,
}

/// One tier.
#[derive(Debug, Clone, Serialize)]
pub struct CrewLadderRow {
    pub tier: String,
    /// Whether `workflow.crew_ladder.tiers` lists the tier.
    pub listed: bool,
    /// The configuration key the pool comes from.
    pub pool_key: String,
    /// The configured pool, canonical.
    pub pool: Vec<CrewPoolEntry>,
    /// The cheapest ladder rung among the pool's enabled, weighted members.
    pub floor: Option<String>,
    /// The rung directly below the floor, which the tier trials.
    pub candidate: Option<String>,
    /// The tier's open trial: the current one, or else the newest open
    /// trial the next sync retires.
    pub trial: Option<CrewLadderTrialView>,
    /// Why the tier has no current trial; absent when it has one.
    pub reason: Option<CrewLadderReason>,
}

/// One ladder trial.
#[derive(Debug, Clone, Serialize)]
pub struct CrewLadderTrialView {
    pub id: String,
    pub name: String,
    pub status: String,
    /// The experiment mode: `shadow` in this phase.
    pub mode: String,
    /// The rung the candidate arm overlays.
    pub candidate: String,
    /// Active arm weights, by arm.
    pub weights: BTreeMap<String, u32>,
    /// Tasks assigned to the candidate arm: in shadow mode, the tasks that
    /// would have run the candidate.
    pub would_be_assignments: usize,
    /// Tasks assigned to the control arm.
    pub control_assignments: usize,
    /// Tasks excluded at admission (another experiment's surface, or an
    /// explicit crew).
    pub excluded: usize,
    pub created_at: String,
    /// Whether this trial is the tier's current one; `false` for a trial
    /// the next sync concludes or abandons.
    pub current: bool,
}

/// Why a tier has no current trial.
#[derive(Debug, Clone, Serialize)]
pub struct CrewLadderReason {
    /// `mode_off`, `tier_not_listed`, `no_ladder_rung`,
    /// `floor_is_bottom_rung`, `operator_experiment`,
    /// `experiments_disabled`, `max_running` or `pending`.
    pub code: String,
    pub detail: String,
    /// The operator experiment covering the tier, for `operator_experiment`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experiment: Option<String>,
}

impl OrbitRuntime {
    /// The ladder report. Read-only: the trial sync at delivery admission
    /// makes the stored trials match it.
    pub fn crew_ladder_report(&self) -> Result<CrewLadderReport, OrbitError> {
        let views = self.experiment_views(None)?;
        let plan = self.crew_ladder_plan(&views, &self.current_crew_ladder_controls())?;
        let rows = plan
            .tiers
            .iter()
            .map(|tier| {
                Ok(CrewLadderRow {
                    tier: tier.tier.to_string(),
                    listed: tier.listed,
                    pool_key: tier.pool_key.clone(),
                    pool: tier.pool.clone(),
                    floor: tier.floor.clone(),
                    candidate: tier.candidate.clone(),
                    trial: self.shown_trial(tier)?,
                    reason: row_reason(tier, &plan.settings, plan.max_running, plan.running),
                })
            })
            .collect::<Result<Vec<_>, OrbitError>>()?;
        let facts = self.sqlite_store()?.list_invocation_accounting_facts(
            &orbit_store::contracts::InvocationAccountingQuery {
                workspace_id: Some(self.stores().jobs().workspace_id().to_string()),
                since: Some(chrono::Utc::now() - chrono::Duration::days(90)),
                until: chrono::Utc::now(),
            },
        )?;
        Ok(CrewLadderReport {
            effort_breakdown: orbit_store::contracts::agent_effort_usage(&facts),
            effort_window_days: 90,
            mode: plan.settings.mode.as_str().to_string(),
            rungs: plan.settings.rungs.clone(),
            tiers: plan
                .settings
                .tiers
                .iter()
                .map(ToString::to_string)
                .collect(),
            explore_rate: plan.settings.explore_rate,
            arm_weights: ladder_arm_weights(&plan.settings),
            experiments_enabled: plan.experiments_enabled,
            max_running: plan.max_running,
            running: plan.running,
            rows,
        })
    }

    /// The kept trial, or else the newest open one, with its assignments.
    fn shown_trial(&self, tier: &TierPlan) -> Result<Option<CrewLadderTrialView>, OrbitError> {
        let kept = match &tier.action {
            TierAction::Keep(id) => tier.open_trials.iter().find(|trial| &trial.view.id == id),
            TierAction::Start | TierAction::Skip(_) => None,
        };
        let Some((trial, current)) = kept
            .map(|trial| (trial, true))
            .or_else(|| tier.open_trials.last().map(|trial| (trial, false)))
        else {
            return Ok(None);
        };
        let rows = self
            .sqlite_store()?
            .experiment_assignments(&self.workspace_id()?, &trial.view.id)?;
        let assigned = |arm: &str| {
            rows.iter()
                .filter(|row| !row.forced && row.arm.as_deref() == Some(arm))
                .count()
        };
        Ok(Some(CrewLadderTrialView {
            id: trial.view.id.clone(),
            name: trial.view.name.clone(),
            status: trial.view.status.clone(),
            mode: trial.view.mode.clone(),
            candidate: trial.candidate.clone(),
            weights: trial.active_weights(),
            would_be_assignments: assigned(CANDIDATE_ARM),
            control_assignments: assigned(CONTROL_ARM),
            excluded: rows
                .iter()
                .filter(|row| row.arm.is_none() && row.exclusion_reason.is_some())
                .count(),
            created_at: trial.view.created_at.clone(),
            current,
        }))
    }
}

fn row_reason(
    tier: &TierPlan,
    settings: &CrewLadderSettings,
    max_running: u32,
    running: usize,
) -> Option<CrewLadderReason> {
    let retiring = tier
        .retired(settings)
        .map(|trial| format!("`{}`", trial.view.name))
        .collect::<Vec<_>>();
    let retiring = if retiring.is_empty() {
        String::new()
    } else {
        format!(
            "; the next delivery admission retires {} with no winner",
            retiring.join(", ")
        )
    };
    let (code, detail, experiment) = match &tier.action {
        TierAction::Keep(_) => return None,
        TierAction::Start => (
            "pending",
            format!(
                "a shadow trial of `{}` starts at the next delivery admission",
                tier.candidate.as_deref().unwrap_or_default()
            ),
            None,
        ),
        TierAction::Skip(reason) => {
            let detail = match reason {
                NoTrial::ModeOff => "workflow.crew_ladder.mode is off".to_string(),
                NoTrial::TierNotListed => {
                    "the tier is not in workflow.crew_ladder.tiers".to_string()
                }
                NoTrial::NoLadderRung => format!(
                    "no rung of workflow.crew_ladder.rungs is an enabled, weighted member of {}",
                    tier.pool_key
                ),
                NoTrial::FloorIsBottomRung => format!(
                    "the floor `{}` is the ladder's bottom rung, so no cheaper rung is left to trial",
                    tier.floor.as_deref().unwrap_or_default()
                ),
                NoTrial::OperatorExperiment { id, name } => format!(
                    "running crew experiment `{name}` ({id}) covers this tier; operator \
                     experiments come first"
                ),
                NoTrial::ExperimentsDisabled => {
                    "experiments.enabled is false, so no trial can start".to_string()
                }
                NoTrial::MaxRunning => format!(
                    "{running} experiment(s) are running and experiments.max_running is \
                     {max_running}; ladder trials count toward it, and tiers earlier in \
                     workflow.crew_ladder.tiers ({}) take free slots first",
                    settings
                        .tiers
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
            let experiment = match reason {
                NoTrial::OperatorExperiment { id, .. } => Some(id.clone()),
                _ => None,
            };
            (reason.code(), detail, experiment)
        }
    };
    Some(CrewLadderReason {
        code: code.to_string(),
        detail: format!("{detail}{retiring}"),
        experiment,
    })
}
