//! Delayed recovery of a pull drain's released authentication incidents.
//!
//! [ORB-17288] An incident belongs to one provider account: the crew that
//! reported it when that crew sets `env` and so runs on its own credential,
//! else the provider's default login. Incidents are probed once per account,
//! the probe runs with that account's crew environment, and a passing probe
//! re-admits only that account's crews.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{Duration, Utc};
use orbit_common::OrbitError;
use orbit_engine::activity_job::cli_runner::{auth_credential_source, run_auth_probe};
use orbit_store::contracts::{ClaimMutation, JobRunQuery, LocalPullAdmission};
use orbit_types::workflow::{Provider, PullAuthExclusion, PullAuthRecovery};

use crate::OrbitRuntime;

const INITIAL_DELAY: Duration = Duration::minutes(10);
const MAX_DELAY: Duration = Duration::minutes(30);

impl OrbitRuntime {
    /// Active authentication incidents across this workspace's live pull drains.
    /// Read-only: doctor never launches a provider or alters a backoff.
    pub fn active_pull_auth_exclusions(&self) -> Result<Vec<PullAuthExclusion>, OrbitError> {
        let mut exclusions = Vec::new();
        for run in self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some(crate::application::contracts::PULL_DRAIN_JOB.into()),
            active_only: true,
            include_steps: false,
            ..Default::default()
        })? {
            if let Some(window) = self.pull_drain_crew_window(&run.run_id)? {
                exclusions.extend(window.auth_exclusions);
            }
        }
        exclusions.sort_by(|a, b| {
            (&a.host, &a.provider, &a.account, a.excluded_at).cmp(&(
                &b.host,
                &b.provider,
                &b.account,
                b.excluded_at,
            ))
        });
        exclusions.dedup();
        Ok(exclusions)
    }

    /// Project an incident without writing it. The leaf's finish time anchors
    /// the first delay, even when the drain or diagnostic reads it much later.
    pub(crate) fn pull_auth_incident(
        &self,
        record: &LocalPullAdmission,
        stored: &BTreeMap<String, PullAuthRecovery>,
    ) -> Result<Option<PullAuthRecovery>, OrbitError> {
        let Some(ClaimMutation::Release(evidence)) = &record.settlement else {
            return Ok(None);
        };
        let Some(unavailable) = &evidence.provider_unavailable else {
            return Ok(None);
        };
        if !self.leaf_reported_authentication(record.leaf_run_id.as_deref()) {
            return Ok(None);
        }
        if let Some(recovery) = stored.get(&record.request.request_id) {
            return Ok(Some(recovery.clone()));
        }
        let Some(crew) = unavailable
            .crew
            .as_deref()
            .and_then(|name| self.context.settings().crews().get(name))
        else {
            return Ok(None);
        };
        let provider = Provider::parse(&crew.assignment.provider)
            .map(|provider| provider.as_str().to_string())
            .unwrap_or_else(|_| crew.assignment.provider.clone());
        let probe = self
            .get_executor_def(&provider)?
            .and_then(|def| def.auth_probe);
        let leaf = record
            .leaf_run_id
            .as_deref()
            .map(|id| self.stores().jobs().get_job_run(id))
            .transpose()?
            .flatten();
        let at = leaf
            .as_ref()
            .and_then(|run| run.finished_at)
            .or_else(|| leaf.as_ref().map(|run| run.created_at))
            .unwrap_or_else(Utc::now);
        let account = crew.account().map(ToOwned::to_owned);
        let relogin_hint = match &account {
            Some(account) => self.account_relogin_hint(account),
            None => probe.as_ref().map_or_else(
                || relogin_hint(&provider),
                |probe| probe.relogin_hint.clone(),
            ),
        };
        Ok(Some(PullAuthRecovery {
            exclusion: PullAuthExclusion {
                provider: provider.clone(),
                account,
                host: record
                    .request
                    .run_context
                    .machine_name
                    .clone()
                    .unwrap_or_else(|| record.destination.execution_machine_id.clone()),
                excluded_at: at,
                error_class: "provider_unavailable/auth".into(),
                relogin_hint,
                credential_source: auth_credential_source(self, &provider, &crew.env),
                next_probe_at: probe.as_ref().map(|_| at + INITIAL_DELAY),
                attempts: 0,
            },
            recovered_at: None,
            recovery_source: None,
        }))
    }

    /// Probe due auth exclusions once per provider account. Reserving the
    /// backoff before spawn makes a crash/retry wait too; capacity and
    /// refusal never enter here.
    pub(crate) fn recover_pull_auth_exclusions(&self, run_id: &str) -> Result<(), OrbitError> {
        let stored = self
            .read_run_state(run_id)?
            .map(|state| state.pull_drain.pull_auth_recovery)
            .unwrap_or_default();
        let mut incidents = BTreeMap::new();
        for record in self.stores().jobs().local_pull_claims_admitted_by(run_id)? {
            if let Some(incident) = self.pull_auth_incident(&record, &stored)? {
                incidents.insert(record.request.request_id, incident);
            }
        }
        if incidents.is_empty() {
            return Ok(());
        }
        self.stores()
            .jobs()
            .update_run_state(run_id, &mut |_, state| {
                for (id, incident) in &incidents {
                    state
                        .pull_drain
                        .pull_auth_recovery
                        .entry(id.clone())
                        .or_insert_with(|| incident.clone());
                }
                Ok(())
            })?;
        let now = Utc::now();
        let due: BTreeSet<(String, Option<String>)> = incidents
            .values()
            .filter(|incident| {
                incident.recovered_at.is_none()
                    && incident.exclusion.next_probe_at.is_some_and(|at| at <= now)
            })
            .map(|incident| {
                (
                    incident.exclusion.provider.clone(),
                    incident.exclusion.account.clone(),
                )
            })
            .collect();
        for (provider, account) in due {
            // An operator may remove the declaration; retain the exclusion.
            let Some(probe) = self
                .get_executor_def(&provider)?
                .and_then(|def| def.auth_probe)
            else {
                continue;
            };
            let ids: Vec<String> = incidents
                .iter()
                .filter(|(_, incident)| {
                    incident.recovered_at.is_none()
                        && incident.exclusion.provider == provider
                        && incident.exclusion.account == account
                })
                .map(|(id, _)| id.clone())
                .collect();
            let mut reserved = false;
            self.stores()
                .jobs()
                .update_run_state(run_id, &mut |_, state| {
                    if state.pull_drain.drain_cancel.is_some()
                        || state.pull_drain.drain_admissions_stop.is_some()
                    {
                        return Ok(());
                    }
                    // Read the current due time under the store lock: a concurrent
                    // pass that reserved this probe wins and this one launches none.
                    if !ids.iter().any(|id| {
                        state
                            .pull_drain
                            .pull_auth_recovery
                            .get(id)
                            .is_some_and(|incident| {
                                incident.recovered_at.is_none()
                                    && incident.exclusion.next_probe_at.is_some_and(|at| at <= now)
                            })
                    }) {
                        return Ok(());
                    }
                    let attempts = ids
                        .iter()
                        .filter_map(|id| state.pull_drain.pull_auth_recovery.get(id))
                        .map(|incident| incident.exclusion.attempts)
                        .max()
                        .unwrap_or(0)
                        .saturating_add(1);
                    let delay = (INITIAL_DELAY * 2_i32.pow(attempts.min(2))).min(MAX_DELAY);
                    for id in &ids {
                        if let Some(incident) = state.pull_drain.pull_auth_recovery.get_mut(id) {
                            incident.exclusion.attempts = attempts;
                            incident.exclusion.next_probe_at = Some(now + delay);
                        }
                    }
                    reserved = true;
                    Ok(())
                })?;
            if !reserved {
                continue;
            }
            let crew_env = self.auth_probe_crew_env(account.as_deref());
            let outcome = run_auth_probe(
                self,
                &provider,
                &crew_env,
                &probe,
                run_id,
                &self.paths().repo_root,
            );
            let passed = outcome.as_ref().is_ok_and(|outcome| outcome.passed);
            let finished = Utc::now();
            self.stores()
                .jobs()
                .update_run_state(run_id, &mut |_, state| {
                    for id in &ids {
                        if let Some(incident) = state.pull_drain.pull_auth_recovery.get_mut(id) {
                            if let Ok(outcome) = &outcome {
                                incident
                                    .exclusion
                                    .credential_source
                                    .clone_from(&outcome.credential_source);
                            }
                            if passed {
                                incident.recovered_at = Some(finished);
                                incident.recovery_source = Some("auth_probe".into());
                                incident.exclusion.next_probe_at = None;
                            }
                        }
                    }
                    Ok(())
                })?;
            let account = account.as_deref().unwrap_or("default");
            tracing::info!(target: "orbit.core.pull", run_id, %provider, account, passed, at = %finished, "authentication recovery probe completed");
            if let Err(error) = outcome {
                tracing::warn!(target: "orbit.core.pull", run_id, %provider, account, %error, "authentication probe failed; exclusion retained with backoff");
            }
        }
        Ok(())
    }
}

impl OrbitRuntime {
    /// The `[crews.<name>] env` a provider account's probe runs with: the
    /// crew account's own, so the probe checks the credential its leaves
    /// used, or none for the provider's default login, which runs on the
    /// shared environment. A crew account this host no longer defines probes
    /// with none.
    fn auth_probe_crew_env(&self, account: Option<&str>) -> Vec<orbit_types::identity::CrewEnvVar> {
        account
            .and_then(|account| self.context.settings().crews().get(account))
            .map(|crew| crew.env.clone())
            .unwrap_or_default()
    }

    /// What renews a crew account's credential: the variables its
    /// `[crews.<name>] env` passes, by name only.
    pub(crate) fn account_relogin_hint(&self, account: &str) -> String {
        let entries = self
            .context
            .settings()
            .crews()
            .get(account)
            .map(|crew| {
                crew.env
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        format!(
            "Renew the credential crew `{account}` runs on: the variables its \
             `[crews.{account}] env` passes ({entries}) on this host."
        )
    }
}

pub(crate) fn relogin_hint(provider: &str) -> String {
    match provider {
        "claude" => "Run `claude auth login`, or renew `claude setup-token` and pass CLAUDE_CODE_OAUTH_TOKEN in [execution.env].pass.".into(),
        "codex" => "Run `codex login`.".into(),
        "gemini" => "Run `gemini` and sign in.".into(),
        "antigravity" => "Run `agy` and sign in.".into(),
        "copilot" => "Run `copilot` and `/login`.".into(),
        "grok" => format!("Run `{provider}` and configure its provider credentials."),
        "muse" => "Run `muse login`, or pass META_API_KEY in [execution.env].pass.".into(),
        _ => format!("Re-login with the `{provider}` CLI."),
    }
}
