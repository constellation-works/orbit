//! Apply trusted Muse implementation outcomes to this task's next crew draw.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::identity::Crew;
use orbit_types::workflow::activity_job::Provider;
use orbit_types::workflow::{
    IMPLEMENT_ONE_STEP_ID, JobRunState, MUSE_MODEL_ACCESS_OUTPUT_KEY, MuseModelAccess,
    TASK_LOCAL_PIPELINE_JOB, TASK_PR_PIPELINE_JOB,
};

use super::draw::auto_task_id;
use crate::OrbitRuntime;

type ModelAccount = (String, Option<String>);

#[derive(Default)]
pub(super) struct MuseModelRefusals(BTreeMap<ModelAccount, DateTime<Utc>>);

impl MuseModelRefusals {
    pub(super) fn rejects_crew_model(&self, crew: &Crew) -> bool {
        is_muse_crew(crew)
            && self.0.keys().any(|(model, account)| {
                model == &crew.assignment.model && account.as_deref() == crew.account()
            })
    }
}

pub(super) fn is_muse_crew(crew: &Crew) -> bool {
    Provider::parse(&crew.assignment.provider).is_ok_and(|provider| provider == Provider::Muse)
}

pub(super) fn model_access_hold(models: &BTreeSet<String>) -> OrbitError {
    let models = models
        .iter()
        .map(|model| format!("`{model}`"))
        .collect::<Vec<_>>()
        .join(", ");
    OrbitError::ProviderModelAccessHold(format!(
        "Muse refused access to {models} in this execution context. No eligible configured crew \
         remains. Select an explicit crew, make an eligible crew available, or run on a host or \
         provider account with access. A verified successful invocation of the model in this \
         context clears its refusal."
    ))
}

impl OrbitRuntime {
    pub(super) fn task_muse_model_refusals(
        &self,
        task_id: &str,
    ) -> Result<MuseModelRefusals, OrbitError> {
        let machine = self.automation_machine_identity();
        let runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            task_id: Some(task_id.to_string()),
            states: vec![JobRunState::Success, JobRunState::Failed, JobRunState::Held],
            include_steps: false,
            input_keys: Some(&["task_id", "task_ids"]),
            ..Default::default()
        })?;
        let run_ids = runs
            .into_iter()
            .filter(|run| {
                matches!(
                    run.job_id.as_str(),
                    TASK_LOCAL_PIPELINE_JOB | TASK_PR_PIPELINE_JOB
                ) && run.input.as_ref().and_then(auto_task_id) == Some(task_id)
                    && run
                        .executed_on
                        .as_ref()
                        .map(|host| host.machine_id.as_str())
                        == machine
            })
            .map(|run| run.run_id)
            .collect::<Vec<_>>();
        if run_ids.is_empty() {
            return Ok(MuseModelRefusals::default());
        }
        let mut refusals = BTreeMap::new();
        let mut successes = BTreeMap::new();
        for (_, state) in self.read_run_states(&run_ids)? {
            let Some(mut state) = state else { continue };
            let Some(evidence) = state
                .pipeline
                .get_mut(IMPLEMENT_ONE_STEP_ID)
                .and_then(serde_json::Value::as_object_mut)
                .and_then(|output| output.remove(MUSE_MODEL_ACCESS_OUTPUT_KEY))
                .and_then(|evidence| serde_json::from_value::<MuseModelAccess>(evidence).ok())
            else {
                continue;
            };
            if evidence.started_at > evidence.finished_at {
                continue;
            }
            let (outcomes, at) = if evidence.refused {
                (&mut refusals, evidence.finished_at)
            } else {
                (&mut successes, evidence.started_at)
            };
            let latest = outcomes
                .entry((evidence.model, evidence.crew_account))
                .or_insert(at);
            *latest = (*latest).max(at);
        }
        refusals.retain(|key, refused_at| {
            successes
                .get(key)
                .is_none_or(|success_at| success_at <= refused_at)
        });
        Ok(MuseModelRefusals(refusals))
    }
}
