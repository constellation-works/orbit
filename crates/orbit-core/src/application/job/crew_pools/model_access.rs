//! Apply trusted Muse implementation outcomes to this task's next crew draw.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{JobRunOrder, JobRunQuery};
use orbit_types::identity::Crew;
use orbit_types::workflow::activity_job::Provider;
use orbit_types::workflow::{
    IMPLEMENT_ONE_STEP_ID, JobRunState, MUSE_MODEL_ACCESS_OUTPUT_KEY, MuseModelAccess,
    TASK_LOCAL_PIPELINE_JOB, TASK_PR_PIPELINE_JOB,
};

use super::draw::auto_task_id;
use crate::OrbitRuntime;

/// How long a verified Muse model refusal excludes that model from the
/// after-landing review pool on the host that observed it. Provider access
/// can change without any run on this host noticing, so the exclusion lapses
/// on its own instead of waiting for a success the exclusion itself prevents.
pub(crate) const MUSE_MODEL_REFUSAL_WINDOW_HOURS: i64 = 12;

/// The most recent runs of each local pipeline job the workspace-wide scan
/// reads; older runs are past the window in any realistic history.
const WORKSPACE_SCAN_RUN_LIMIT: usize = 200;

type ModelAccount = (String, Option<String>);

#[derive(Default)]
pub(crate) struct MuseModelRefusals(BTreeMap<ModelAccount, DateTime<Utc>>);

impl MuseModelRefusals {
    pub(crate) fn rejects_crew_model(&self, crew: &Crew) -> bool {
        self.refused_at(crew).is_some()
    }

    /// When the provider last refused `crew`'s exact model on its account.
    pub(crate) fn refused_at(&self, crew: &Crew) -> Option<DateTime<Utc>> {
        if !is_muse_crew(crew) {
            return None;
        }
        self.0
            .iter()
            .find(|((model, account), _)| {
                model == &crew.assignment.model && account.as_deref() == crew.account()
            })
            .map(|(_, refused_at)| *refused_at)
    }
}

pub(crate) fn is_muse_crew(crew: &Crew) -> bool {
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
        self.muse_model_refusals_in_runs(&run_ids, None)
    }

    /// Muse model refusals the owner-local implementation runs of any task
    /// recorded in the last [`MUSE_MODEL_REFUSAL_WINDOW_HOURS`] hours, net of
    /// a newer verified success of the same model and account. Only runs this
    /// host executed count, so a follower's claimed leaf, another host and
    /// another provider account cannot exclude a model here.
    pub(crate) fn workspace_muse_model_refusals(
        &self,
        now: DateTime<Utc>,
    ) -> Result<MuseModelRefusals, OrbitError> {
        let Some(machine) = self.automation_machine_identity() else {
            return Ok(MuseModelRefusals::default());
        };
        let since = now - Duration::hours(MUSE_MODEL_REFUSAL_WINDOW_HOURS);
        let mut run_ids = Vec::new();
        for job_id in [TASK_LOCAL_PIPELINE_JOB, TASK_PR_PIPELINE_JOB] {
            let runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
                job_id: Some(job_id.to_string()),
                states: vec![JobRunState::Success, JobRunState::Failed, JobRunState::Held],
                include_steps: false,
                input_keys: Some(&[]),
                recency_since: Some(since),
                order_by: JobRunOrder::Recency,
                limit: Some(WORKSPACE_SCAN_RUN_LIMIT),
                ..Default::default()
            })?;
            run_ids.extend(
                runs.into_iter()
                    .filter(|run| {
                        run.executed_on
                            .as_ref()
                            .is_some_and(|host| host.machine_id == machine)
                    })
                    .map(|run| run.run_id),
            );
        }
        if run_ids.is_empty() {
            return Ok(MuseModelRefusals::default());
        }
        self.muse_model_refusals_in_runs(&run_ids, Some(since))
    }

    /// Fold the verified implementation evidence of `run_ids`: the latest
    /// refusal of each model and account, unless a success of it is newer.
    /// With `since`, evidence that finished earlier is ignored.
    fn muse_model_refusals_in_runs(
        &self,
        run_ids: &[String],
        since: Option<DateTime<Utc>>,
    ) -> Result<MuseModelRefusals, OrbitError> {
        let mut refusals = BTreeMap::new();
        let mut successes = BTreeMap::new();
        for (_, state) in self.read_run_states(run_ids)? {
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
            if evidence.started_at > evidence.finished_at
                || since.is_some_and(|since| evidence.finished_at < since)
            {
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
