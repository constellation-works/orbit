//! Per-host provider authentication failures for the drain card.
//!
//! A `provider_unavailable` step is an authentication failure. The card groups
//! those runs by the host that executed them and the provider that failed.
//! A successful run of that same provider on that same host drops the group
//! when it started after the failure. Nothing is written: the projection is
//! the recent job history.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{JobRunOrder, JobRunQuery};
use orbit_types::task::TaskComplexity;
use orbit_types::workflow::{
    JobRun, JobRunState, PipelineState, Provider, ProviderFailureClass, failed_provider,
};
use serde::Serialize;
use serde_json::Value;

use super::auth_recovery::relogin_hint;
use crate::OrbitRuntime;
use crate::application::job::crew_pools::complexity_crews_key;

/// How far back the drain card looks for an authentication failure or the
/// success that clears it. Older runs are not shown.
const WINDOW: Duration = Duration::days(7);

/// Most-recent runs read for each side of the projection. A busy host can
/// push an older failure or its clearing success past this page.
pub(crate) const SCAN_LIMIT: usize = 200;

/// One run that failed authentication and has not been cleared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProviderAuthFailureRun {
    pub run_id: String,
    pub at: DateTime<Utc>,
}

/// Authentication failures of one provider on one host, still uncleared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProviderAuthFailure {
    /// Display name of the machine that executed the runs, or its id when
    /// the run recorded no name.
    pub host: String,
    /// Canonical provider (`claude`, `codex`).
    pub provider: String,
    /// The failing runs, oldest first.
    pub runs: Vec<ProviderAuthFailureRun>,
    /// What the operator runs to renew this provider's credentials.
    pub fix_command: String,
}

impl OrbitRuntime {
    /// Authentication failures still standing at `now`.
    ///
    /// A store error yields an empty list and a warning, the same way a
    /// provider-limit read does, so one bad scan cannot blank readiness.
    pub(crate) fn provider_auth_failures(&self, now: DateTime<Utc>) -> Vec<ProviderAuthFailure> {
        match self.project_provider_auth_failures(now) {
            Ok(items) => items,
            Err(error) => {
                tracing::warn!("readiness shows no provider auth failures: {error}");
                Vec::new()
            }
        }
    }

    fn project_provider_auth_failures(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<ProviderAuthFailure>, OrbitError> {
        let since = now - WINDOW;
        let failures = self.stores().jobs().list_job_runs_filtered(&bounded_query(
            JobRunState::Failed,
            since,
            true,
        ))?;
        if !failures.iter().any(|run| {
            run.steps.iter().any(|step| {
                ProviderFailureClass::of(step.error_code.as_deref(), step.error_message.as_deref())
                    == Some(ProviderFailureClass::Unavailable)
            })
        }) {
            return Ok(Vec::new());
        }
        let successes = self.stores().jobs().list_job_runs_filtered(&bounded_query(
            JobRunState::Success,
            since,
            false,
        ))?;
        let crews = self.recorded_run_crews(failures.iter().chain(successes.iter()))?;
        let fallback_host = self.fallback_host();
        let mut cleared_at: BTreeMap<(String, String), DateTime<Utc>> = BTreeMap::new();
        for run in &successes {
            for crew in crews.crews(run, None) {
                let Some(provider) = self.provider_of_crew(&crew) else {
                    continue;
                };
                let key = (host_of(run, &fallback_host), provider);
                let at = clearing_at(run);
                cleared_at
                    .entry(key)
                    .and_modify(|latest| {
                        if at > *latest {
                            *latest = at;
                        }
                    })
                    .or_insert(at);
            }
        }

        let mut open: BTreeMap<(String, String), Vec<ProviderAuthFailureRun>> = BTreeMap::new();
        for run in &failures {
            for provider in self.unavailable_providers(run, &crews) {
                let at = run_at(run);
                let key = (host_of(run, &fallback_host), provider);
                if cleared_at.get(&key).is_some_and(|cleared| *cleared > at) {
                    continue;
                }
                open.entry(key).or_default().push(ProviderAuthFailureRun {
                    run_id: run.run_id.clone(),
                    at,
                });
            }
        }

        let mut items = Vec::new();
        for ((host, provider), mut runs) in open {
            runs.sort_by(|left, right| left.at.cmp(&right.at).then(left.run_id.cmp(&right.run_id)));
            if runs.is_empty() {
                continue;
            }
            items.push(ProviderAuthFailure {
                host,
                provider: provider.clone(),
                runs,
                fix_command: self.fix_command(&provider),
            });
        }
        Ok(items)
    }

    /// The provider the authentication step names, else the recorded agent
    /// assignments of that failure. Ordinary jobs use their resolved crew.
    fn unavailable_providers(&self, run: &JobRun, crews: &RunCrewLookup) -> Vec<String> {
        let Some(step) = run
            .steps
            .iter()
            .filter(|step| {
                ProviderFailureClass::of(step.error_code.as_deref(), step.error_message.as_deref())
                    == Some(ProviderFailureClass::Unavailable)
            })
            .min_by_key(|step| step.step_index)
        else {
            return Vec::new();
        };
        if let Some(named) = step.error_message.as_deref().and_then(failed_provider) {
            return vec![canonical_provider(named)];
        }
        crews
            .crews(run, step.error_message.as_deref())
            .into_iter()
            .filter_map(|crew| self.provider_of_crew(&crew))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Read special jobs' assignments in one bounded batch. Without such a
    /// job in the projection, no pipeline state is read.
    pub(crate) fn recorded_run_crews<'a>(
        &self,
        runs: impl Iterator<Item = &'a JobRun>,
    ) -> Result<RunCrewLookup, OrbitError> {
        let ids = runs
            .filter(|run| uses_recorded_crews(&run.job_id))
            .map(|run| run.run_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(RunCrewLookup(HashMap::new()));
        }
        Ok(RunCrewLookup(
            self.read_run_states(&ids)?
                .into_iter()
                .filter_map(|(id, state)| state.map(|state| (id, state)))
                .collect(),
        ))
    }

    pub(crate) fn provider_of_crew(&self, name: &str) -> Option<String> {
        let crew = self.context.settings().crews().get(name)?;
        Some(canonical_provider(&crew.assignment.provider))
    }

    fn fix_command(&self, provider: &str) -> String {
        let declared = self
            .get_executor_def(provider)
            .ok()
            .flatten()
            .and_then(|def| def.auth_probe)
            .map(|probe| probe.relogin_hint.trim().to_string())
            .filter(|hint| !hint.is_empty());
        declared.unwrap_or_else(|| relogin_hint(provider))
    }

    pub(crate) fn fallback_host(&self) -> String {
        let Some(location) = self.automation_execution_location() else {
            return "this host".to_string();
        };
        named(&location.machine_name).unwrap_or_else(|| {
            let id = location.machine_id.trim();
            if id.is_empty() {
                "this host".to_string()
            } else {
                id.to_string()
            }
        })
    }
}

pub(crate) fn bounded_query(
    state: JobRunState,
    since: DateTime<Utc>,
    include_steps: bool,
) -> JobRunQuery {
    JobRunQuery {
        state: Some(state),
        created_since: Some(since),
        limit: Some(SCAN_LIMIT),
        order_by: JobRunOrder::Recency,
        include_steps,
        ..JobRunQuery::default()
    }
}

/// When a run ended: its finish, else its start, else its row's creation.
pub(crate) fn run_at(run: &JobRun) -> DateTime<Utc> {
    run.finished_at.or(run.started_at).unwrap_or(run.created_at)
}

/// When a success shows that the provider works. A run that started before a
/// failure used the credentials that failed, even if it finished after it.
pub(crate) fn clearing_at(run: &JobRun) -> DateTime<Utc> {
    run.started_at.unwrap_or(run.created_at)
}

pub(crate) fn host_of(run: &JobRun, fallback: &str) -> String {
    run.executed_on
        .as_ref()
        .and_then(|location| {
            named(&location.machine_name).or_else(|| {
                let id = location.machine_id.trim();
                (!id.is_empty()).then(|| id.to_string())
            })
        })
        .unwrap_or_else(|| fallback.to_string())
}

fn named(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

pub(crate) fn canonical_provider(raw: &str) -> String {
    let trimmed = raw.trim();
    Provider::parse(trimmed)
        .map(|provider| provider.as_str().to_string())
        .unwrap_or_else(|_| trimmed.to_ascii_lowercase())
}

/// Recorded agent assignments for jobs whose task crew is not their agent crew.
/// Both readiness and routine holds use this lookup, including failure-to-partition
/// attribution for finding verification.
pub(crate) struct RunCrewLookup(HashMap<String, PipelineState>);

impl RunCrewLookup {
    pub(crate) fn crews(&self, run: &JobRun, failure: Option<&str>) -> Vec<String> {
        if !uses_recorded_crews(&run.job_id) {
            return run.resolved_crew.iter().cloned().collect();
        }
        let Some(state) = self.0.get(&run.run_id) else {
            return Vec::new();
        };
        let key = match run.job_id.as_str() {
            "task_pilot_pipeline" => Some(complexity_crews_key(TaskComplexity::Low)),
            "blocked_task_recovery_pipeline" => Some("workflow.final_recovery_crews".to_string()),
            _ => None,
        };
        if let Some(key) = key {
            return state
                .activity_crew_draws
                .get(&key)
                .map(|draw| vec![draw.crew.clone()])
                .unwrap_or_default();
        }

        // Verifiers draw per partition, not from one run-wide pool. A prepared
        // assignment alone is not proof that its agent ran. Successful runs
        // count partitions with results; failed runs count provider errors
        // actually carried by their terminal diagnostic, in either round.
        let mut crews = BTreeSet::new();
        for (partitions, results, errors) in [
            ("/prepare/partitions", "/verdict_results", "/verdict_errors"),
            (
                "/apply/escalation_prepared/partitions",
                "/escalation_results",
                "/escalation_errors",
            ),
        ] {
            let Some(partitions) = state.pipeline.pointer(partitions).and_then(Value::as_array)
            else {
                continue;
            };
            for (index, partition) in partitions.iter().enumerate() {
                let ran = match failure {
                    Some(message) => state
                        .pipeline
                        .pointer(errors)
                        .and_then(|errors| errors.get(index))
                        .and_then(Value::as_str)
                        .is_some_and(|error| {
                            ProviderFailureClass::of(None, Some(error)).is_some()
                                && ProviderFailureClass::of(None, Some(error))
                                    == ProviderFailureClass::of(None, Some(message))
                                && failed_provider(error) == failed_provider(message)
                                && message.contains(error)
                        }),
                    None if run.state == JobRunState::Success => state
                        .pipeline
                        .pointer(results)
                        .and_then(|results| results.get(index))
                        .is_some_and(Value::is_object),
                    None => false,
                };
                if ran && let Some(crew) = partition.get("crew").and_then(Value::as_str) {
                    crews.insert(crew.to_string());
                }
            }
        }
        crews.into_iter().collect()
    }
}

fn uses_recorded_crews(job: &str) -> bool {
    matches!(
        job,
        "task_pilot_pipeline" | "blocked_task_recovery_pipeline" | "finding_verification_pipeline"
    )
}
