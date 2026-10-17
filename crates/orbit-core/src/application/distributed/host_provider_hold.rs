//! Host-scoped provider failure holds for the task-pilot routine and the crew
//! draw it shares with other complexity-pool activities.
//!
//! A `provider_unavailable` or `provider_capacity` failure on this host holds
//! the crews a pilot would draw, for that class's base backoff, without a new
//! stored record. The window is the failure's own finish time: the projection
//! carries no backoff of its own. A success of the provider on this host that
//! started after the failure, or a pull-drain auth probe that records
//! `recovered_at`, clears unavailability. A success that started before the
//! failure ran on what then failed, so finishing later does not clear it.
//! Capacity clears only when such a success is of the held crew, or of any
//! crew of the provider when the hold covers the whole provider. A probe does
//! not clear capacity. Limit and refusal failures are
//! not holds here.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{JobRunOrder, JobRunQuery};
use orbit_types::identity::Crew;
use orbit_types::workflow::{JobRun, JobRunState, ProviderFailureClass, failed_provider};

use super::PULL_DRAIN_JOB;
use super::auth_failures::{canonical_provider, clearing_at, host_of};
use crate::OrbitRuntime;
use crate::runtime::task::provider_hold::base_backoff;

/// How far back a failure or the success that clears it is read. The longest
/// base backoff honoured here is 30 minutes; the extra half hour keeps a
/// failure whose row was created slightly before it finished.
const LOOKBACK: Duration = Duration::hours(1);

/// Most-recent runs read for each side of the projection.
const SCAN_LIMIT: usize = 200;

/// One standing unavailable or capacity hold on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostProviderHold {
    pub(crate) class: ProviderFailureClass,
    /// Canonical provider (`claude`, `codex`).
    pub(crate) provider: String,
    /// `None` covers every crew of `provider`. A capacity hold names the run's
    /// crew when that crew's provider is the one that failed.
    pub(crate) crew: Option<String>,
    pub(crate) not_before: DateTime<Utc>,
    pub(crate) run_id: String,
}

impl HostProviderHold {
    /// Whether `crew` is excluded at `now`.
    pub(crate) fn covers(&self, crew: &Crew, now: DateTime<Utc>) -> bool {
        if now >= self.not_before {
            return false;
        }
        if canonical_provider(&crew.assignment.provider) != self.provider {
            return false;
        }
        self.crew.as_deref().is_none_or(|name| name == crew.name)
    }

    /// `provider_unavailable <subject> until <time> (<run>)`. The subject is
    /// the crew when the hold names one, otherwise the provider.
    pub(crate) fn detail(&self) -> String {
        let subject = self.crew.as_deref().unwrap_or(self.provider.as_str());
        let until = self.not_before.to_rfc3339_opts(SecondsFormat::Secs, true);
        format!(
            "{} {subject} until {until} ({})",
            self.class.as_str(),
            self.run_id
        )
    }
}

#[derive(Clone)]
struct OpenFailure {
    at: DateTime<Utc>,
    run_id: String,
    class: ProviderFailureClass,
    provider: String,
    crew: Option<String>,
}

impl OrbitRuntime {
    /// Unavailable and capacity holds still standing on this host at `now`.
    ///
    /// A store error is logged and read as no holds. The projection is
    /// advisory: a missed scan must not stop every routine, and the provider
    /// still refuses a run it cannot serve.
    pub(crate) fn standing_host_provider_holds(&self, now: DateTime<Utc>) -> Vec<HostProviderHold> {
        match self.project_host_provider_holds(now) {
            Ok(holds) => holds,
            Err(error) => {
                tracing::warn!("routine reads no host provider holds: {error}");
                Vec::new()
            }
        }
    }

    fn project_host_provider_holds(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<HostProviderHold>, OrbitError> {
        let since = now - LOOKBACK;
        let failures = self.stores().jobs().list_job_runs_filtered(&bounded_query(
            Some(JobRunState::Failed),
            Some(since),
            true,
        ))?;
        let successes = self.stores().jobs().list_job_runs_filtered(&bounded_query(
            Some(JobRunState::Success),
            Some(since),
            false,
        ))?;
        let fallback = self.fallback_host();
        let aliases = self.host_aliases(&fallback);
        let mut provider_success: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
        let mut crew_success: BTreeMap<(String, String), DateTime<Utc>> = BTreeMap::new();
        for run in &successes {
            if !on_this_host(run, &aliases, &fallback) {
                continue;
            }
            let Some(crew_name) = run.resolved_crew.as_deref() else {
                continue;
            };
            let Some(provider) = self.provider_of_crew(crew_name) else {
                continue;
            };
            let at = clearing_at(run);
            note_later(&mut provider_success, provider.clone(), at);
            note_later(&mut crew_success, (provider, crew_name.to_string()), at);
        }
        // A pull drain is long-lived: its row is created when the window
        // opens, and a probe records `recovered_at` on that same row later.
        // Matching it by `created_at` would miss a recovery on a drain that
        // started before the lookback. Probes clear unavailability only;
        // they stay out of the success maps capacity reads.
        let mut provider_probe: BTreeMap<String, DateTime<Utc>> = BTreeMap::new();
        self.note_probe_recoveries(&aliases, &fallback, &mut provider_probe)?;

        let mut open: BTreeMap<(String, String, Option<String>), OpenFailure> = BTreeMap::new();
        for run in &failures {
            if !on_this_host(run, &aliases, &fallback) {
                continue;
            }
            let Some(failure) = self.classify_hold(run) else {
                continue;
            };
            if hold_cleared(&failure, &provider_success, &crew_success, &provider_probe)
                || now >= failure.at + base_backoff(failure.class)
            {
                continue;
            }
            let key = (
                failure.class.as_str().to_string(),
                failure.provider.clone(),
                failure.crew.clone(),
            );
            match open.get_mut(&key) {
                Some(existing)
                    if failure.at > existing.at
                        || (failure.at == existing.at
                            && failure.run_id.as_str() > existing.run_id.as_str()) =>
                {
                    existing.at = failure.at;
                    existing.run_id = failure.run_id.clone();
                }
                Some(_) => {}
                None => {
                    open.insert(key, failure);
                }
            }
        }

        let mut holds = open
            .into_values()
            .map(|failure| HostProviderHold {
                class: failure.class,
                provider: failure.provider,
                crew: failure.crew,
                not_before: failure.at + base_backoff(failure.class),
                run_id: failure.run_id,
            })
            .collect::<Vec<_>>();
        holds.sort_by(|left, right| {
            left.class
                .as_str()
                .cmp(right.class.as_str())
                .then(left.provider.cmp(&right.provider))
                .then(left.crew.cmp(&right.crew))
                .then(left.run_id.cmp(&right.run_id))
        });
        Ok(holds)
    }

    /// The earliest unavailable or capacity step on `run`, scoped the way a
    /// host hold is: unavailability covers every crew of that provider, and
    /// capacity covers the run's crew when that crew uses the failing provider.
    fn classify_hold(&self, run: &JobRun) -> Option<OpenFailure> {
        let step = run
            .steps
            .iter()
            .filter(|step| {
                matches!(
                    ProviderFailureClass::of(
                        step.error_code.as_deref(),
                        step.error_message.as_deref()
                    ),
                    Some(ProviderFailureClass::Unavailable | ProviderFailureClass::Capacity)
                )
            })
            .min_by_key(|step| step.step_index)?;
        let class =
            ProviderFailureClass::of(step.error_code.as_deref(), step.error_message.as_deref())?;
        let named = step
            .error_message
            .as_deref()
            .and_then(failed_provider)
            .map(canonical_provider);
        let crew_provider = run
            .resolved_crew
            .as_deref()
            .and_then(|crew| self.provider_of_crew(crew));
        let provider = named.or(crew_provider.clone())?;
        let crew = match class {
            ProviderFailureClass::Capacity => match (&run.resolved_crew, &crew_provider) {
                (Some(name), Some(crew_provider)) if crew_provider == &provider => {
                    Some(name.clone())
                }
                _ => None,
            },
            ProviderFailureClass::Unavailable => None,
            ProviderFailureClass::Limit | ProviderFailureClass::Refusal => return None,
        };
        Some(OpenFailure {
            at: run_at(run),
            run_id: run.run_id.clone(),
            class,
            provider,
            crew,
        })
    }

    fn note_probe_recoveries(
        &self,
        aliases: &BTreeSet<String>,
        fallback: &str,
        provider_probe: &mut BTreeMap<String, DateTime<Utc>>,
    ) -> Result<(), OrbitError> {
        let mut seen = BTreeSet::new();
        for run in self
            .pull_drain_runs_by_recency()?
            .into_iter()
            .chain(self.active_pull_drain_runs()?)
        {
            if !seen.insert(run.run_id.clone()) || !on_this_host(&run, aliases, fallback) {
                continue;
            }
            let state = match self.read_run_state(&run.run_id) {
                Ok(state) => state,
                Err(error) => {
                    tracing::warn!(
                        run_id = %run.run_id,
                        "could not read pull-drain auth recovery: {error}"
                    );
                    continue;
                }
            };
            let Some(state) = state else {
                continue;
            };
            for recovery in state.pull_drain.pull_auth_recovery.values() {
                let Some(recovered_at) = recovery.recovered_at else {
                    continue;
                };
                if !aliases.contains(&recovery.exclusion.host) {
                    continue;
                }
                note_later(
                    provider_probe,
                    canonical_provider(&recovery.exclusion.provider),
                    recovered_at,
                );
            }
        }
        Ok(())
    }

    fn pull_drain_runs_by_recency(&self) -> Result<Vec<JobRun>, OrbitError> {
        self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some(PULL_DRAIN_JOB.to_string()),
            limit: Some(SCAN_LIMIT),
            order_by: JobRunOrder::Recency,
            include_steps: false,
            ..JobRunQuery::default()
        })
    }

    fn active_pull_drain_runs(&self) -> Result<Vec<JobRun>, OrbitError> {
        self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some(PULL_DRAIN_JOB.to_string()),
            active_only: true,
            limit: Some(SCAN_LIMIT),
            order_by: JobRunOrder::Recency,
            include_steps: false,
            ..JobRunQuery::default()
        })
    }

    fn host_aliases(&self, fallback: &str) -> BTreeSet<String> {
        let mut aliases = BTreeSet::new();
        aliases.insert(fallback.to_string());
        let Some(location) = self.automation_execution_location() else {
            return aliases;
        };
        if let Some(name) = location
            .machine_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            aliases.insert(name.to_string());
        }
        let id = location.machine_id.trim();
        if !id.is_empty() {
            aliases.insert(id.to_string());
        }
        aliases
    }
}

fn bounded_query(
    state: Option<JobRunState>,
    since: Option<DateTime<Utc>>,
    include_steps: bool,
) -> JobRunQuery {
    JobRunQuery {
        state,
        created_since: since,
        limit: Some(SCAN_LIMIT),
        order_by: JobRunOrder::Recency,
        include_steps,
        ..JobRunQuery::default()
    }
}

fn on_this_host(run: &JobRun, aliases: &BTreeSet<String>, fallback: &str) -> bool {
    aliases.contains(&host_of(run, fallback))
}

fn run_at(run: &JobRun) -> DateTime<Utc> {
    run.finished_at.or(run.started_at).unwrap_or(run.created_at)
}

/// A success that started, or a probe that recovered, strictly after the
/// failure finished clears it. Equal timestamps do not.
fn later_than(at: DateTime<Utc>, cleared: Option<&DateTime<Utc>>) -> bool {
    cleared.is_some_and(|cleared| *cleared > at)
}

fn hold_cleared(
    failure: &OpenFailure,
    provider_success: &BTreeMap<String, DateTime<Utc>>,
    crew_success: &BTreeMap<(String, String), DateTime<Utc>>,
    provider_probe: &BTreeMap<String, DateTime<Utc>>,
) -> bool {
    match failure.class {
        ProviderFailureClass::Capacity => match failure.crew.as_deref() {
            Some(crew) => later_than(
                failure.at,
                crew_success.get(&(failure.provider.clone(), crew.to_string())),
            ),
            None => later_than(failure.at, provider_success.get(&failure.provider)),
        },
        ProviderFailureClass::Unavailable => {
            later_than(failure.at, provider_success.get(&failure.provider))
                || later_than(failure.at, provider_probe.get(&failure.provider))
        }
        ProviderFailureClass::Limit | ProviderFailureClass::Refusal => false,
    }
}

fn note_later<K: Ord>(map: &mut BTreeMap<K, DateTime<Utc>>, key: K, at: DateTime<Utc>) {
    map.entry(key)
        .and_modify(|latest| {
            if at > *latest {
                *latest = at;
            }
        })
        .or_insert(at);
}
