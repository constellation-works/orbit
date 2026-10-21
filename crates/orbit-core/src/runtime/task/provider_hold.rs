//! Hold a task back from the provider that failed its run [ORB-14266].
//!
//! A local run that ends with `[provider_capacity]`, `[provider_unavailable]`,
//! `[provider_limit]` or `[provider_refusal]` failed on the provider, not on
//! the work. Run finalization moves its task to `backlog` under a
//! `provider_failure_hold` history entry instead of blocking it
//! ([`OrbitRuntime::provider_failure_hold`] builds the hold). [ORB-15573] A
//! claimed leaf its provider refused releases its claim, and the owner
//! records the same hold with the release
//! ([`OrbitRuntime::claimed_provider_failure_hold`]). Capacity and
//! unavailability exclude the crew the run used; a refusal excludes every
//! crew of the refusing provider, since the content would be refused again on
//! any of them. A usage limit belongs to the provider account, so it excludes
//! every crew of that provider too, or only those on the model the limit
//! names [ORB-14695]. The exclusion lasts until the hold's `not_before`: the
//! reset a limit reported, else a backoff that grows with each consecutive
//! hold.
//!
//! The hold is the task's latest status decision; any later status change
//! supersedes it. While it stands, admission draws the task's crew from the
//! crews it does not exclude — its own crew or pool, then its complexity
//! pool, then the workspace default — and defers the task when every one is
//! excluded (`application::task::provider_hold`).
//!
//! [ORB-17288] A crew that sets `env` is its own provider account. A failure
//! names that account when its crew is one (`account=<crew>` after the
//! provider), and a usage limit then excludes only that crew, while one on
//! the provider's default login excludes only the provider's crews without
//! `env`. A failure that names no account, every one written before accounts
//! were named included, is the default login's. A stored hold keeps the
//! crews it excluded, so one placed before this reads as it always did.
//!
//! [ORB-16379] Unavailability is the failing host's: an unavailable hold
//! names this host's machine and excludes its crews only here, so pull
//! admission still offers the task to a follower on that crew. The failed
//! crews are also recorded as this host's [`ProviderCrewOutage`]s, which
//! outlive the held task's status: until that failure's backoff ends, this
//! host's local admission draws no task onto them, whether or not the held
//! task is still in the backlog.

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_engine::TaskAutomationUpdate;
use orbit_types::identity::Crew;
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    JobRun, PROVIDER_FAILURE_HOLD_EVENT, Provider, ProviderCrewOutage, ProviderFailureClass,
    ProviderFailureHold, ProviderLimitFailure, failed_account, failed_provider,
    provider_account_label,
};

use crate::OrbitRuntime;
use crate::runtime::run_input::non_empty;

/// How long a hold's first exclusion lasts, by class. Each consecutive hold
/// within [`BACKOFF_MEMORY`] doubles it, up to [`max_backoff`]. A refusal is
/// not transient: another provider may take the task at once, and the
/// refusing one is not asked again for a day. A usage limit that reported its
/// reset is held until then instead.
pub(crate) fn base_backoff(class: ProviderFailureClass) -> Duration {
    match class {
        ProviderFailureClass::Capacity => Duration::minutes(15),
        ProviderFailureClass::Unavailable | ProviderFailureClass::Limit => Duration::minutes(30),
        ProviderFailureClass::Refusal => Duration::hours(24),
    }
}

/// The longest a backoff grows to, by class.
fn max_backoff(class: ProviderFailureClass) -> Duration {
    match class {
        ProviderFailureClass::Limit => Duration::hours(6),
        _ => Duration::hours(24),
    }
}

const BACKOFF_MEMORY: Duration = Duration::hours(24);

/// [ORB-14695] The bounds a usage limit's reported reset is clamped to.
const MIN_LIMIT_HOLD: Duration = Duration::minutes(5);
const MAX_LIMIT_HOLD: Duration = Duration::days(7);

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

    /// [`Self::standing_provider_hold`] as `machine` sees it, for admission:
    /// an unreadable history is logged and read as no hold, so the task is
    /// drawn as usual.
    pub(crate) fn provider_hold_on(
        &self,
        task: &Task,
        machine: Option<&str>,
    ) -> Option<ProviderFailureHold> {
        self.standing_provider_hold(task, Utc::now())
            .unwrap_or_else(|error| {
                tracing::warn!(task_id = %task.id, "could not read provider failure hold: {error}");
                None
            })?
            .on_machine(machine)
    }

    /// The hold a run that failed with `class` places on `task`.
    ///
    /// The failing provider is the one the failure names, else the run
    /// crew's. A refusal excludes every configured crew of that provider;
    /// capacity and unavailability exclude the run's crew, or every crew of
    /// the provider when the failing step ran on a provider other than the
    /// run crew's (a reviewer, say). A usage limit excludes every crew whose
    /// provider parses to the same one, or only those on the model the limit
    /// names when that matches some of them. [ORB-17288] Only crews of the
    /// failing account count for a limit and for a step on another provider:
    /// the crew the failure names as its account, else the provider's crews
    /// without `env`. A hold that still stands from an earlier run keeps its
    /// crews excluded, so successive failures narrow the draw.
    ///
    /// A usage limit's hold lasts until the reset the provider reported,
    /// clamped to between [`MIN_LIMIT_HOLD`] and [`MAX_LIMIT_HOLD`] from
    /// `now`. One that reported none backs off from 30 minutes, doubling for
    /// each limit hold on the same provider in the last day, up to 6 hours.
    ///
    /// [ORB-16379] An unavailable hold names this host's machine. The crews
    /// this failure excluded are recorded as this host's outages until the
    /// failure's own backoff ends; an outage that cannot be recorded is
    /// logged, and the hold still stands.
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
            .and_then(serde_json::Value::as_str);
        let machine = (class == ProviderFailureClass::Unavailable)
            .then(|| self.automation_machine_identity())
            .flatten();
        let (hold, failure) = self.failure_hold(
            task,
            &run.run_id,
            run_crew,
            class,
            error_message,
            machine,
            now,
        )?;
        if class == ProviderFailureClass::Unavailable {
            for crew in failure.excluded_crews {
                let outage = ProviderCrewOutage {
                    provider: failure.provider.clone(),
                    crew,
                    until: failure.not_before,
                    observed_at: now,
                    run_id: run.run_id.clone(),
                };
                if let Err(error) = self.record_provider_crew_outage(&outage) {
                    tracing::warn!(
                        task_id = %task.id,
                        crew = %outage.crew,
                        "could not record this host's provider outage: {error}"
                    );
                }
            }
        }
        Ok(hold)
    }

    /// [`Self::provider_failure_hold`] for a run this host does not hold: a
    /// claimed leaf, named by its `run_id` and the crew it ran as. The hold
    /// names no machine.
    pub(crate) fn claimed_provider_failure_hold(
        &self,
        task: &Task,
        run_id: &str,
        run_crew: Option<&str>,
        class: ProviderFailureClass,
        error_message: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<ProviderFailureHold, OrbitError> {
        self.failure_hold(task, run_id, run_crew, class, error_message, None, now)
            .map(|(hold, _)| hold)
    }

    /// The hold a failure places, scoped to `machine` when one is given, and
    /// what this failure alone excluded, before an earlier standing hold was
    /// absorbed.
    #[allow(clippy::too_many_arguments)]
    fn failure_hold(
        &self,
        task: &Task,
        run_id: &str,
        run_crew: Option<&str>,
        class: ProviderFailureClass,
        error_message: Option<&str>,
        machine: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(ProviderFailureHold, ProviderFailureHold), OrbitError> {
        let run_crew = run_crew
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
        // [ORB-17288] The account that failed: the crew the failure names,
        // else the provider's default login.
        let account = error_message
            .and_then(failed_account)
            .map(ToOwned::to_owned);
        let on_account = |crew: &&Crew| crew.account() == account.as_deref();
        let provider_crews = |accounts_only: bool| -> Vec<String> {
            registry
                .values()
                .filter(|crew| Some(&crew.assignment.provider) == provider.as_ref())
                .filter(|crew| !accounts_only || on_account(crew))
                .map(|crew| crew.name.clone())
                .collect()
        };
        let limit = (class == ProviderFailureClass::Limit)
            .then(|| error_message.and_then(ProviderLimitFailure::from_text))
            .flatten()
            .unwrap_or_default();
        let mut excluded = match class {
            // A content refusal is the provider's policy, whichever account
            // asked.
            ProviderFailureClass::Refusal => provider_crews(false),
            ProviderFailureClass::Limit => provider.as_deref().map_or_else(Vec::new, |provider| {
                limit_crews(
                    registry.values().filter(on_account),
                    provider,
                    limit.model.as_deref(),
                )
            }),
            ProviderFailureClass::Capacity | ProviderFailureClass::Unavailable => {
                if provider.is_some() && run_provider != provider {
                    provider_crews(true)
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
            .filter(|entry| {
                // [ORB-14695] A limit backs off only from earlier limit holds
                // on the same provider.
                class != ProviderFailureClass::Limit
                    || entry
                        .note
                        .as_deref()
                        .and_then(ProviderFailureHold::from_text)
                        .is_some_and(|hold| {
                            hold.class == ProviderFailureClass::Limit
                                && same_provider(hold.provider.as_deref(), provider.as_deref())
                                && hold.account == account
                        })
            })
            .count();
        let backoff =
            (base_backoff(class) * 2_i32.pow(recent.min(6) as u32)).min(max_backoff(class));
        let backoff = backoff.max(base_backoff(class));
        let not_before = match limit.resets_at {
            Some(resets_at) => resets_at.clamp(now + MIN_LIMIT_HOLD, now + MAX_LIMIT_HOLD),
            None => now + backoff,
        };
        excluded.sort();
        excluded.dedup();
        let failure = ProviderFailureHold {
            class,
            provider,
            account,
            excluded_crews: excluded,
            machine: machine.map(ToOwned::to_owned),
            carried_crews: Vec::new(),
            not_before,
            run_id: run_id.to_string(),
        };
        let mut hold = failure.clone();
        if let Some(previous) = latest_hold(&history).filter(|hold| hold.stands_at(now)) {
            hold.absorb(previous);
        }
        Ok((hold, failure))
    }
}

/// Whether two provider labels parse to the same provider; unparsable
/// labels compare as written.
pub(crate) fn same_provider(left: Option<&str>, right: Option<&str>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => match (Provider::parse(left), Provider::parse(right)) {
            (Ok(left), Ok(right)) => left == right,
            _ => left == right,
        },
        _ => false,
    }
}

/// [ORB-14695] The configured crews a usage limit on `provider` excludes:
/// every crew whose provider parses to the same one, or, when the limit names
/// a model that some of them run, only those. `crews` are those of the
/// failing account.
fn limit_crews<'a>(
    crews: impl Iterator<Item = &'a Crew>,
    provider: &str,
    model: Option<&str>,
) -> Vec<String> {
    let crews = crews
        .filter(|crew| same_provider(Some(&crew.assignment.provider), Some(provider)))
        .collect::<Vec<_>>();
    let scoped = model
        .map(str::to_ascii_lowercase)
        .map(|model| {
            crews
                .iter()
                .filter(|crew| crew.assignment.model.to_ascii_lowercase().contains(&model))
                .map(|crew| crew.name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if scoped.is_empty() {
        crews.iter().map(|crew| crew.name.clone()).collect()
    } else {
        scoped
    }
}

/// Move a task whose run its provider failed to `backlog` under `hold`.
pub(crate) fn provider_failure_hold_update(
    job_id: &str,
    hold: &ProviderFailureHold,
) -> TaskAutomationUpdate {
    let provider = hold.provider.as_deref().map_or_else(
        || "its provider".to_string(),
        |provider| provider_account_label(provider, hold.account.as_deref()),
    );
    TaskAutomationUpdate {
        status: Some(TaskStatus::Backlog),
        status_event: Some(PROVIDER_FAILURE_HOLD_EVENT.to_string()),
        status_note: Some(hold.text(&format!(
            "workflow run held: job={job_id}, run_id={}; {} on {provider}, so the task waits in \
             the backlog with crews {} excluded until {}; another crew may take it before then",
            hold.run_id,
            hold.class.as_str(),
            hold.describe_crews(),
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
