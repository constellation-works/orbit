//! Hold a task back from the provider that failed its run [ORB-14266].
//!
//! A local run that ends with `[provider_capacity]`, `[provider_unavailable]`
//! or `[provider_refusal]` failed on the provider, not on the work. Run
//! finalization moves its task to `backlog` under a `provider_failure_hold`
//! history entry instead of blocking it ([`OrbitRuntime::provider_failure_hold`]
//! builds the hold). Capacity and unavailability exclude the crew the run
//! used; a refusal excludes every crew of the refusing provider, since the
//! content would be refused again on any of them. The exclusion lasts until
//! the hold's `not_before`, which backs off with each consecutive hold.
//!
//! The hold is the task's latest status decision; any later status change
//! supersedes it. While it stands, admission draws the task's crew from the
//! crews it does not exclude — its own crew or pool, then its complexity
//! pool, then the workspace default — and defers the task when every one is
//! excluded (`application::task::provider_hold`).

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_engine::TaskAutomationUpdate;
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    JobRun, PROVIDER_FAILURE_HOLD_EVENT, ProviderFailureClass, ProviderFailureHold, failed_provider,
};

use crate::OrbitRuntime;
use crate::runtime::run_input::non_empty;

/// How long a hold's first exclusion lasts, by class. Each consecutive hold
/// within [`BACKOFF_MEMORY`] doubles it, up to [`MAX_BACKOFF`]. A refusal is
/// not transient: another provider may take the task at once, and the
/// refusing one is not asked again for a day.
fn base_backoff(class: ProviderFailureClass) -> Duration {
    match class {
        ProviderFailureClass::Capacity => Duration::minutes(15),
        ProviderFailureClass::Unavailable => Duration::minutes(30),
        ProviderFailureClass::Refusal => Duration::hours(24),
    }
}

const MAX_BACKOFF: Duration = Duration::hours(24);
const BACKOFF_MEMORY: Duration = Duration::hours(24);

impl OrbitRuntime {
    /// The hold `task`'s latest status decision put it under, if it still
    /// stands at `now`.
    pub(crate) fn standing_provider_hold(
        &self,
        task: &Task,
        now: DateTime<Utc>,
    ) -> Result<Option<ProviderFailureHold>, OrbitError> {
        if task.status != TaskStatus::Backlog {
            return Ok(None);
        }
        let history = self.get_task_history(&task.id)?;
        Ok(latest_status_hold(&history).filter(|hold| hold.stands_at(now)))
    }

    /// [`Self::standing_provider_hold`] for admission: an unreadable history
    /// is logged and read as no hold, so the task is drawn as usual.
    pub(crate) fn admission_provider_hold(&self, task: &Task) -> Option<ProviderFailureHold> {
        self.standing_provider_hold(task, Utc::now())
            .unwrap_or_else(|error| {
                tracing::warn!(task_id = %task.id, "could not read provider failure hold: {error}");
                None
            })
    }

    /// The hold a run that failed with `class` places on `task`.
    ///
    /// The failing provider is the one the failure names, else the run
    /// crew's. A refusal excludes every configured crew of that provider;
    /// capacity and unavailability exclude the run's crew, or every crew of
    /// the provider when the failing step ran on a provider other than the
    /// run crew's (a reviewer, say). A hold that still stands from an earlier
    /// run keeps its crews excluded, so successive failures narrow the draw.
    pub(crate) fn provider_failure_hold(
        &self,
        task: &Task,
        run: &JobRun,
        class: ProviderFailureClass,
        error_message: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<ProviderFailureHold, OrbitError> {
        let run_crew = run
            .input
            .as_ref()
            .and_then(|input| input.get("crew"))
            .and_then(serde_json::Value::as_str)
            .and_then(non_empty)
            .map(ToOwned::to_owned)
            .or_else(|| {
                task.crew
                    .as_deref()
                    .and_then(non_empty)
                    .map(ToOwned::to_owned)
            })
            .or_else(|| {
                self.lookup_crew_for_task(None, None)
                    .ok()
                    .map(|crew| crew.name)
            });
        let registry = self.context.settings().crews();
        let run_provider = run_crew
            .as_deref()
            .and_then(|crew| registry.get(crew))
            .map(|crew| crew.assignment.provider.clone());
        let provider = error_message
            .and_then(failed_provider)
            .map(ToOwned::to_owned)
            .or_else(|| run_provider.clone());
        let provider_crews = || -> Vec<String> {
            registry
                .values()
                .filter(|crew| Some(&crew.assignment.provider) == provider.as_ref())
                .map(|crew| crew.name.clone())
                .collect()
        };
        let mut excluded = match class {
            ProviderFailureClass::Refusal => provider_crews(),
            ProviderFailureClass::Capacity | ProviderFailureClass::Unavailable => {
                if provider.is_some() && run_provider != provider {
                    provider_crews()
                } else {
                    Vec::new()
                }
            }
        };
        if excluded.is_empty()
            && let Some(crew) = &run_crew
        {
            excluded.push(crew.clone());
        }

        let history = self.get_task_history(&task.id)?;
        let recent = history
            .iter()
            .filter(|entry| {
                entry.event == PROVIDER_FAILURE_HOLD_EVENT && now - entry.at < BACKOFF_MEMORY
            })
            .count();
        let backoff = (base_backoff(class) * 2_i32.pow(recent.min(6) as u32)).min(MAX_BACKOFF);
        let backoff = backoff.max(base_backoff(class));
        let mut not_before = now + backoff;
        if let Some(previous) = latest_hold(&history).filter(|hold| hold.stands_at(now)) {
            excluded.extend(previous.excluded_crews);
            not_before = not_before.max(previous.not_before);
        }
        excluded.sort();
        excluded.dedup();
        Ok(ProviderFailureHold {
            class,
            provider,
            excluded_crews: excluded,
            not_before,
            run_id: run.run_id.clone(),
        })
    }
}

/// Move a task whose run its provider failed to `backlog` under `hold`.
pub(crate) fn provider_failure_hold_update(
    job_id: &str,
    hold: &ProviderFailureHold,
) -> TaskAutomationUpdate {
    let provider = hold.provider.as_deref().unwrap_or("its provider");
    TaskAutomationUpdate {
        status: Some(TaskStatus::Backlog),
        status_event: Some(PROVIDER_FAILURE_HOLD_EVENT.to_string()),
        status_note: Some(hold.text(&format!(
            "workflow run held: job={job_id}, run_id={}; {} on {provider}, so the task waits in \
             the backlog with crews {} excluded until {}; another crew may take it before then",
            hold.run_id,
            hold.class.as_str(),
            hold.excluded_crews.join(", "),
            hold.not_before.to_rfc3339()
        ))),
        ..TaskAutomationUpdate::default()
    }
}

/// The hold the task's latest status decision put it under, if it was one.
fn latest_status_hold(history: &[TaskHistoryEntry]) -> Option<ProviderFailureHold> {
    let entry = history
        .iter()
        .rev()
        .find(|entry| entry.to_status.is_some())?;
    if entry.event != PROVIDER_FAILURE_HOLD_EVENT || entry.to_status != Some(TaskStatus::Backlog) {
        return None;
    }
    ProviderFailureHold::from_text(entry.note.as_deref()?)
}

/// The most recent hold in the task's history, superseded or not.
fn latest_hold(history: &[TaskHistoryEntry]) -> Option<ProviderFailureHold> {
    history
        .iter()
        .rev()
        .filter(|entry| entry.event == PROVIDER_FAILURE_HOLD_EVENT)
        .find_map(|entry| ProviderFailureHold::from_text(entry.note.as_deref()?))
}

/// Whether `history`'s latest status decision is the hold `run_id` placed.
pub(crate) fn held_by_run(history: &[TaskHistoryEntry], run_id: &str) -> bool {
    latest_status_hold(history).is_some_and(|hold| hold.run_id == run_id)
}
