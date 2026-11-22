//! Apply trusted Muse implementation outcomes to automatic crew draws.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{JobRunOrder, JobRunQuery};
use orbit_types::identity::Crew;
use orbit_types::workflow::activity_job::Provider;
use orbit_types::workflow::{
    CLAIMED_DELIVERY_LEAF_JOBS, IMPLEMENT_ONE_STEP_ID, JobRunState, MUSE_MODEL_ACCESS_OUTPUT_KEY,
    MuseModelAccess, TASK_LOCAL_PIPELINE_JOB, TASK_PR_PIPELINE_JOB,
};

use super::draw::auto_task_id;
use crate::OrbitRuntime;

/// How long a verified Muse model refusal excludes that model on the host and
/// provider account that observed it (for automatic implementation draws and
/// the after-landing review pool). Provider access can change without any run
/// on this host noticing, so the exclusion lapses on its own instead of
/// waiting for a success the exclusion itself prevents.
pub(crate) const MUSE_MODEL_REFUSAL_WINDOW_HOURS: i64 = 12;

/// The most recent runs of each local pipeline job the workspace-wide scan
/// reads; older runs are past the window in any realistic history.
const WORKSPACE_SCAN_RUN_LIMIT: usize = 200;

type ModelAccount = (String, Option<String>);

#[derive(Clone, Default)]
pub(crate) struct MuseModelRefusals(BTreeMap<ModelAccount, DateTime<Utc>>);

thread_local! {
    static MODEL_ACCESS_PASS_DEPTH: Cell<u32> = const { Cell::new(0) };
    static MODEL_ACCESS_PASS: RefCell<Option<MuseModelRefusals>> = const { RefCell::new(None) };
}

/// Caches the workspace-wide Muse model-access scan lazily across one backlog
/// snapshot so admission does not re-query run history per backlog task.
#[must_use = "dropping the guard ends the admission model-access pass"]
pub(crate) struct AdmissionModelAccessPass;

impl AdmissionModelAccessPass {
    /// Enter a snapshot pass. A nested pass reuses the outer cache.
    pub(crate) fn enter() -> Self {
        let depth = MODEL_ACCESS_PASS_DEPTH.with(|depth| {
            let next = depth.get().saturating_add(1);
            depth.set(next);
            next
        });
        if depth == 1 {
            MODEL_ACCESS_PASS.with(|state| *state.borrow_mut() = None);
        }
        Self
    }
}

impl Drop for AdmissionModelAccessPass {
    fn drop(&mut self) {
        MODEL_ACCESS_PASS_DEPTH.with(|depth| {
            let next = depth.get().saturating_sub(1);
            depth.set(next);
            if next == 0 {
                MODEL_ACCESS_PASS.with(|state| *state.borrow_mut() = None);
            }
        });
    }
}

impl MuseModelRefusals {
    pub(crate) fn rejects_crew_model(&self, crew: &Crew) -> bool {
        self.refused_at(crew).is_some()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
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
    /// Muse model refusals this follower's own claimed leaves of `task_id`
    /// recorded in the last [`MUSE_MODEL_REFUSAL_WINDOW_HOURS`] hours, net of
    /// a newer verified success of the same model and account.
    ///
    /// Only claimed delivery leaves this host executed count: the owner's
    /// runs, another follower's leaves and this host's owner-local runs of a
    /// same-id local task are not evidence about this host's account for the
    /// claimed task. A host with no automation identity reads none. The
    /// window lets a refusal lapse, as on the owner's after-landing review
    /// pool, because the exclusion itself prevents the success that would
    /// clear it.
    pub(super) fn claimed_leaf_muse_model_refusals(
        &self,
        task_id: &str,
        now: DateTime<Utc>,
    ) -> Result<MuseModelRefusals, OrbitError> {
        let Some(machine) = self.automation_machine_identity() else {
            return Ok(MuseModelRefusals::default());
        };
        let since = now - Duration::hours(MUSE_MODEL_REFUSAL_WINDOW_HOURS);
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
                CLAIMED_DELIVERY_LEAF_JOBS.contains(&run.job_id.as_str())
                    && run.input.as_ref().and_then(auto_task_id) == Some(task_id)
                    && run
                        .executed_on
                        .as_ref()
                        .is_some_and(|host| host.machine_id == machine)
            })
            .map(|run| run.run_id)
            .collect::<Vec<_>>();
        if run_ids.is_empty() {
            return Ok(MuseModelRefusals::default());
        }
        self.muse_model_refusals_in_runs(&run_ids, Some(since))
    }

    /// Workspace-wide recent Muse model refusals, cached lazily for the active
    /// [`AdmissionModelAccessPass`] so backlog snapshots query run history once.
    pub(crate) fn pass_workspace_muse_model_refusals(
        &self,
    ) -> Result<MuseModelRefusals, OrbitError> {
        let in_pass = MODEL_ACCESS_PASS_DEPTH.with(|depth| depth.get()) > 0;
        if in_pass && let Some(cached) = MODEL_ACCESS_PASS.with(|state| state.borrow().clone()) {
            return Ok(cached);
        }
        let refusals = self.workspace_muse_model_refusals(Utc::now())?;
        if in_pass {
            MODEL_ACCESS_PASS.with(|state| *state.borrow_mut() = Some(refusals.clone()));
        }
        Ok(refusals)
    }

    /// Muse model refusals the owner-local implementation runs of any task
    /// recorded in the last [`MUSE_MODEL_REFUSAL_WINDOW_HOURS`] hours, net of
    /// a newer verified success of the same model and account. Only runs with
    /// this host's machine identity (or unattributed local runs when this
    /// runtime has none) count, so a follower's claimed leaf, another host and
    /// another provider account cannot exclude a model here.
    pub(crate) fn workspace_muse_model_refusals(
        &self,
        now: DateTime<Utc>,
    ) -> Result<MuseModelRefusals, OrbitError> {
        let machine = self.automation_machine_identity();
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
                            .map(|host| host.machine_id.as_str())
                            == machine
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
