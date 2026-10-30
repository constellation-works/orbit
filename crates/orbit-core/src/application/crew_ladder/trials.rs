//! The trial sync: reconcile `system:crew-ladder` experiments with the plan
//! through the experiment lifecycle. A trial whose candidate is no longer
//! the tier's is concluded with no winner (or abandoned while a draft), and
//! a tier that may trial and has none gets one, created and started in
//! experiment `shadow` mode.
//!
//! The sync runs at delivery admission, before experiment assignment, so a
//! task is assigned against trials that match the ladder in force.

use orbit_common::OrbitError;
use orbit_config::{CrewLadderMode, CrewLadderSettings};
use serde_json::json;

use super::CREW_LADDER_ACTOR;
use super::plan::{
    CANDIDATE_ARM, CONTROL_ARM, LadderPlan, LadderTrial, TierAction, TierPlan,
    ladder_trial_population,
};
use crate::OrbitRuntime;
use crate::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput, ExperimentView,
};

/// The metric a trial is read on until the ladder's own quality score
/// exists.
const TRIAL_PRIMARY_METRIC: &str = "first_pass_acceptance";

impl OrbitRuntime {
    /// Reconcile the ladder trials with the current plan. A workspace with
    /// the ladder off and no open trial reads its experiments and writes
    /// nothing. One tier's refused write is logged and does not stop the
    /// others; the next admission retries it.
    pub(crate) fn sync_crew_ladder_trials(&self) -> Result<(), OrbitError> {
        let views = self.experiment_views(None)?;
        let any_open = views
            .iter()
            .filter_map(LadderTrial::from_view)
            .any(|trial| trial.is_open());
        if self.crew_ladder_settings().mode == CrewLadderMode::Off && !any_open {
            return Ok(());
        }
        let plan = self.crew_ladder_plan(&views)?;
        for tier in &plan.tiers {
            for trial in tier.retired(&plan.settings) {
                if let Err(error) = self.retire_ladder_trial(trial, tier, &plan.settings) {
                    tracing::warn!(
                        experiment = %trial.view.id,
                        tier = %tier.tier,
                        %error,
                        "crew ladder could not retire a trial; the next admission retries"
                    );
                }
            }
        }
        for tier in plan
            .tiers
            .iter()
            .filter(|tier| tier.action == TierAction::Start)
        {
            if let Err(error) = self.start_ladder_trial(tier, &plan, &views) {
                tracing::warn!(
                    tier = %tier.tier,
                    %error,
                    "crew ladder could not start a trial; the next admission retries"
                );
            }
        }
        Ok(())
    }

    /// Conclude a started trial with no winner, or abandon a draft.
    fn retire_ladder_trial(
        &self,
        trial: &LadderTrial,
        tier: &TierPlan,
        settings: &CrewLadderSettings,
    ) -> Result<(), OrbitError> {
        let reason = match (&tier.action, &tier.candidate) {
            (TierAction::Skip(reason), _) => format!(
                "crew ladder: {} runs no trial ({})",
                tier.tier,
                reason.code()
            ),
            (_, Some(candidate)) if *candidate != trial.candidate => format!(
                "crew ladder: {} now trials `{candidate}`, not `{}`",
                tier.tier, trial.candidate
            ),
            (_, candidate) if !trial.matches_ladder(candidate.as_ref(), settings) => format!(
                "crew ladder: {}'s trial of `{}` no longer matches workflow.crew_ladder \
                 explore_rate or exclude_types",
                tier.tier, trial.candidate
            ),
            _ => format!(
                "crew ladder: {} already has a trial for `{}`",
                tier.tier, trial.candidate
            ),
        };
        let change = if trial.view.status == "draft" {
            ExperimentChange::Abandon
        } else {
            ExperimentChange::Conclude { arm: None }
        };
        self.change_experiment(&trial.view.id, change, ladder_actor(reason))?;
        Ok(())
    }

    /// Create (or reuse a draft) and start the tier's trial. A start the
    /// lifecycle refuses leaves no draft behind.
    fn start_ladder_trial(
        &self,
        tier: &TierPlan,
        plan: &LadderPlan,
        views: &[ExperimentView],
    ) -> Result<(), OrbitError> {
        let (Some(floor), Some(candidate)) = (&tier.floor, &tier.candidate) else {
            return Ok(());
        };
        let id = match tier.reusable_draft(&plan.settings) {
            Some(draft) => draft.view.id.clone(),
            None => {
                let name = next_trial_name(views, tier);
                let input = trial_definition(tier, plan, floor, candidate);
                let reason = format!(
                    "crew ladder: trial `{candidate}` below {}'s floor",
                    tier.tier
                );
                self.create_experiment(&name, input, ladder_actor(reason))?
                    .id
            }
        };
        let reason = format!(
            "crew ladder: shadow trial of `{candidate}` for {}",
            tier.tier
        );
        if let Err(error) =
            self.change_experiment(&id, ExperimentChange::Start, ladder_actor(reason))
        {
            if self.experiment_view(&id)?.status == "draft" {
                self.change_experiment(
                    &id,
                    ExperimentChange::Abandon,
                    ladder_actor(format!("crew ladder: start refused: {error}")),
                )?;
            }
            return Err(error);
        }
        Ok(())
    }
}

fn ladder_actor(reason: String) -> ExperimentActor {
    ExperimentActor {
        actor: CREW_LADDER_ACTOR.to_string(),
        reason: Some(reason),
    }
}

/// `crew-ladder-<tier>-<n>`, one past the highest `n` any experiment of
/// that prefix holds, so a name is never reused across trials.
fn next_trial_name(views: &[ExperimentView], tier: &TierPlan) -> String {
    let prefix = format!("crew-ladder-{}-", tier.tier);
    let last = views
        .iter()
        .filter_map(|view| view.name.strip_prefix(&prefix)?.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("{prefix}{}", last + 1)
}

/// The trial's definition: control at `1 − explore_rate`, the candidate's
/// one-rung crew overlay at `explore_rate`, the tier's tasks minus
/// `exclude_types`, in experiment `shadow` mode.
fn trial_definition(
    tier: &TierPlan,
    plan: &LadderPlan,
    floor: &str,
    candidate: &str,
) -> ExperimentDefinitionInput {
    let (control_weight, candidate_weight) = plan.settings.arm_weights();
    ExperimentDefinitionInput {
        name: None,
        hypothesis: Some(format!(
            "Crew ladder: `{candidate}`, the rung below {tier}'s floor `{floor}`, holds {tier} \
             quality within {margin} points at a lower cost.",
            tier = tier.tier,
            margin = plan.settings.margin,
        )),
        arms: Some(json!([
            { "name": CONTROL_ARM, "control": true, "weight": control_weight },
            {
                "name": CANDIDATE_ARM,
                "weight": candidate_weight,
                "overlay": { "crew": { "pool": [{ "crew": candidate, "weight": 1 }] } },
            },
        ])),
        population: Some(ladder_trial_population(tier.tier, &plan.settings)),
        primary_metric: Some(TRIAL_PRIMARY_METRIC.to_string()),
        guardrails: None,
        min_sample: None,
        max_days: None,
        mode: Some("shadow".to_string()),
    }
}
