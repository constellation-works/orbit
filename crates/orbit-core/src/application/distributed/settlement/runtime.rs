//! The runtime reads behind a pull drain's settlement reports and crew
//! window, and how an owner's answer to a pull is classified.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_common::{OrbitError, RemoteToolCode};
use orbit_store::contracts::{ClaimMutation, LocalPullPhase};
use orbit_types::workflow::{
    CrewExclusion, CrewExclusionSource, PullAuthExclusion, PullCrewPreflight,
};

use super::crew_window::{
    CapacityFailures, OwnerBusyReleases, OwnerBusyVerdict, PullCrewWindow, phase_guidance,
    phase_name,
};
use super::entries::{PullLeafClaim, RefusedPullSettlement};
use crate::application::contracts::{ClaimedLeafStage, DrainClaimedLeaf};

/// A raw weekly pressure as milli-units for the pull capability
/// [ORB-17773]. Non-finite or negative readings read neutral; the clamp
/// keeps the conversion exact and the ordering key bounded.
fn pressure_milli(pressure: f64) -> u64 {
    if !pressure.is_finite() || pressure < 0.0 {
        return orbit_types::task::NEUTRAL_PRESSURE_MILLI;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        (pressure * 1000.0).round().clamp(0.0, 1.0e12) as u64
    }
}

impl crate::OrbitRuntime {
    /// The admissions a pull drain carries that still hold a slot: every one
    /// it made itself and, while it is live, those for its owner whose own
    /// drain has ended — it took them over. Never one another live drain
    /// made, so cancelling one drain cannot touch another's work. Empty for
    /// every other run.
    pub(crate) fn pull_drain_admissions(
        &self,
        drain_run_id: &str,
    ) -> Result<Vec<orbit_store::contracts::LocalPullAdmission>, OrbitError> {
        let jobs = self.stores().jobs();
        let Some(run) = jobs.get_job_run(drain_run_id)? else {
            return Ok(Vec::new());
        };
        if run.job_id != crate::application::contracts::PULL_DRAIN_JOB {
            return Ok(Vec::new());
        }
        let inherits = (!run.state.is_terminal())
            .then(|| {
                run.input
                    .as_ref()
                    .and_then(|input| input.get("destination"))
                    .cloned()
                    .and_then(|value| {
                        serde_json::from_value::<orbit_store::contracts::PullDestination>(value)
                            .ok()
                    })
            })
            .flatten();
        let mut carried = Vec::new();
        for record in jobs.unsettled_local_pull_admissions()? {
            let admitted_by = &record.request.run_context.run_id;
            let own = admitted_by == drain_run_id;
            if own
                || (inherits.as_ref() == Some(&record.destination)
                    && jobs
                        .get_job_run(admitted_by)?
                        .is_none_or(|admitter| admitter.state.is_terminal()))
            {
                carried.push(record);
            }
        }
        Ok(carried)
    }

    /// The workspace capacity a pull drain competes for, from the same reading
    /// as the allocator. Includes admissions without a live leaf and leaves
    /// belonging to other drains; reviewing claims earn capped admission credit.
    /// `None` for every other run. Reading capacity never writes state.
    pub fn pull_drain_occupancy(
        &self,
        drain_run_id: &str,
    ) -> Result<Option<orbit_store::contracts::DrainLeafOccupancy>, OrbitError> {
        let jobs = self.stores().jobs();
        match jobs.get_job_run(drain_run_id)? {
            Some(run) if run.job_id == crate::application::contracts::PULL_DRAIN_JOB => {
                jobs.drain_leaf_occupancy().map(Some)
            }
            _ => Ok(None),
        }
    }

    /// The claimed leaves a pull drain is still carrying: launched and not
    /// yet terminal. Empty for every other run.
    pub fn pull_drain_claimed_leaves(
        &self,
        drain_run_id: &str,
    ) -> Result<Vec<DrainClaimedLeaf>, OrbitError> {
        use LocalPullPhase as Phase;
        let jobs = self.stores().jobs();
        let mut leaves = Vec::new();
        for record in self.pull_drain_admissions(drain_run_id)? {
            if !matches!(
                record.phase,
                Phase::Launching | Phase::Launched | Phase::Settling
            ) {
                continue;
            }
            let Some(leaf) = record.leaf_run_id.as_deref() else {
                continue;
            };
            let Some(run) = jobs.get_job_run(leaf)? else {
                continue;
            };
            if run.state.is_terminal() {
                continue;
            }
            let stage = if self
                .read_run_state(leaf)?
                .is_some_and(|state| state.in_landing_review())
            {
                ClaimedLeafStage::Reviewing
            } else {
                ClaimedLeafStage::Implementing
            };
            leaves.push(DrainClaimedLeaf {
                leaf_run_id: run.run_id.clone(),
                job_id: run.job_id.clone(),
                task_id: record
                    .receipt
                    .as_ref()
                    .and_then(|receipt| receipt.claim.as_ref())
                    .map(|claim| claim.task_id.clone()),
                owner: record.destination.selector.clone(),
                admitted_by: record.request.run_context.run_id.clone(),
                leaf_state: run.state.to_string(),
                settlement_phase: phase_name(record.phase),
                stage,
            });
        }
        Ok(leaves)
    }

    /// The claimed leaf stage for one task, from this host's unsettled
    /// pull admissions: the leaf's stage, run id and state. This is the
    /// task-keyed form of [`Self::pull_drain_claimed_leaves`] for a reader
    /// (the task detail projection) that holds a task, not a drain run.
    /// `None` when this host holds no live admission for the task — the
    /// caller must not guess a stage for a run it cannot read.
    pub fn claimed_leaf_stage_for_task(
        &self,
        task_id: &str,
    ) -> Result<Option<(ClaimedLeafStage, String, String)>, OrbitError> {
        use LocalPullPhase as Phase;
        let jobs = self.stores().jobs();
        for record in jobs.unsettled_local_pull_admissions()? {
            if !matches!(
                record.phase,
                Phase::Launching | Phase::Launched | Phase::Settling
            ) {
                continue;
            }
            let claimed_task = record
                .receipt
                .as_ref()
                .and_then(|receipt| receipt.claim.as_ref())
                .map(|claim| claim.task_id.as_str());
            if claimed_task != Some(task_id) {
                continue;
            }
            let Some(leaf) = record.leaf_run_id.as_deref() else {
                continue;
            };
            let Some(run) = jobs.get_job_run(leaf)? else {
                continue;
            };
            if run.state.is_terminal() {
                continue;
            }
            let stage = if self
                .read_run_state(leaf)?
                .is_some_and(|state| state.in_landing_review())
            {
                ClaimedLeafStage::Reviewing
            } else {
                ClaimedLeafStage::Implementing
            };
            return Ok(Some((stage, run.run_id.clone(), run.state.to_string())));
        }
        Ok(None)
    }

    /// The settlements a pull drain carries that its owner refused while
    /// still holding their claims [ORB-13979]. Empty for every other run.
    pub fn pull_drain_refused_settlements(
        &self,
        drain_run_id: &str,
    ) -> Result<Vec<RefusedPullSettlement>, OrbitError> {
        Ok(self
            .pull_drain_admissions(drain_run_id)?
            .iter()
            .filter_map(RefusedPullSettlement::of)
            .collect())
    }

    /// The crew window of a pull drain run; `None` for every other run.
    pub fn pull_drain_crew_window(
        &self,
        drain_run_id: &str,
    ) -> Result<Option<PullCrewWindow>, OrbitError> {
        let jobs = self.stores().jobs();
        match jobs.get_job_run(drain_run_id)? {
            Some(run) if run.job_id == crate::application::contracts::PULL_DRAIN_JOB => {}
            _ => return Ok(None),
        }
        let preflight = self
            .read_run_state(drain_run_id)?
            .and_then(|state| state.pull_drain.pull_crew_preflight);
        self.crew_window_from(drain_run_id, preflight).map(Some)
    }

    /// [`Self::pull_drain_crew_window`] over a preflight the caller holds,
    /// for a pass that could not persist the one it just took.
    pub(crate) fn crew_window_from(
        &self,
        drain_run_id: &str,
        preflight: Option<PullCrewPreflight>,
    ) -> Result<PullCrewWindow, OrbitError> {
        let mut excluded = preflight
            .as_ref()
            .map(|preflight| preflight.excluded.clone())
            .unwrap_or_default();
        let stored_recovery = self
            .read_run_state(drain_run_id)?
            .map(|state| state.pull_drain.pull_auth_recovery)
            .unwrap_or_default();
        let mut auth_exclusions = Vec::new();
        let mut capacity = BTreeMap::<String, CapacityFailures>::new();
        let mut host_suppressed = None;
        let mut owner_busy = OwnerBusyReleases::default();
        let default_crew = match &preflight {
            Some(preflight) => preflight.default_crew.clone(),
            None => self
                .context
                .settings()
                .default_crew()
                .map(ToOwned::to_owned),
        };
        for record in self
            .stores()
            .jobs()
            .local_pull_claims_admitted_by(drain_run_id)?
        {
            let Some(ClaimMutation::Release(evidence)) = &record.settlement else {
                continue;
            };
            let task = record
                .receipt
                .as_ref()
                .and_then(|receipt| receipt.claim.as_ref())
                .map(|claim| claim.task_id.as_str())
                .unwrap_or("a claimed task");
            // The owner's own busy store blames neither this host nor the
            // crew: it backs the drain off for a bounded time below, and
            // suppresses the host only past its episode bound.
            if let Some(failure) = &evidence.failure
                && failure.owner_busy
            {
                // A leaf whose run is no longer recorded here ended long
                // ago: its release still counts towards the episode bound,
                // and its backoff has passed. A failed read fails the window
                // instead, so it cannot add an episode.
                let at = self
                    .leaf_ended_at(record.leaf_run_id.as_deref())?
                    .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
                owner_busy.record(at, format!("{task} was released: {}", failure.reason));
                continue;
            }
            if let Some(failure) = &evidence.failure
                && failure.class.suppresses_host()
                && host_suppressed.is_none()
            {
                host_suppressed = Some(format!(
                    "{task} was released ({}): {}",
                    failure.class.as_str(),
                    failure.reason
                ));
            }
            // [ORB-14257] A provider release keeps its own source; any other
            // released failure whose class blames this host excludes the crew
            // too. [ORB-14262] Authentication is the provider's login, not the
            // crew's model, so every configured crew of that provider is
            // excluded; [ORB-17288] of that provider account, that is: a crew
            // that sets `env` is its own account and alone excluded, and a
            // crew without one excludes the provider's other crews without
            // one. Capacity stays on the named crew: another model may
            // still have room. Provider labels are parsed, so `anthropic`
            // groups with `claude`. [ORB-14697] A usage limit excludes nothing
            // here: the leaf recorded it in this host's provider-limit store,
            // whose reading excludes the provider's crews below until it
            // lapses. [ORB-15791] Capacity is transient too, so it backs the
            // crew off for a bounded time below rather than for the window.
            let authentication = evidence.provider_unavailable.is_some()
                && self.leaf_reported_authentication(record.leaf_run_id.as_deref());
            if authentication
                && let Some(incident) = self.pull_auth_incident(&record, &stored_recovery)?
            {
                if incident.recovered_at.is_some() {
                    continue;
                }
                if !auth_exclusions.iter().any(|entry: &PullAuthExclusion| {
                    entry.provider == incident.exclusion.provider
                        && entry.account == incident.exclusion.account
                }) {
                    auth_exclusions.push(incident.exclusion);
                }
            }
            if !authentication
                && !evidence
                    .failure
                    .as_ref()
                    .is_some_and(|failure| failure.provider_limit)
                && let Some(unavailable) = &evidence.provider_unavailable
                && let Some(crew) = unavailable.crew.as_deref()
                && let Some(at) = self.leaf_ended_at(record.leaf_run_id.as_deref())?
            {
                capacity
                    .entry(crew.to_string())
                    .or_default()
                    .record(at, format!("{task} failed: {}", unavailable.reason));
                continue;
            }
            let (crews, source, reason) = match (&evidence.provider_unavailable, &evidence.failure)
            {
                (Some(_), Some(failure)) if failure.provider_limit && !authentication => continue,
                (Some(unavailable), _) => {
                    let crews = match unavailable.crew.as_deref() {
                        Some(crew) if authentication => self.crews_sharing_account(crew),
                        Some(crew) => vec![crew.to_string()],
                        None => Vec::new(),
                    };
                    (
                        crews,
                        CrewExclusionSource::ProviderUnavailable,
                        format!("{task} failed: {}", unavailable.reason),
                    )
                }
                // A leaf whose task names no crew ran as the window's
                // default, even when it died before resolving one.
                // [ORB-14634] A forge outage is neither the crew's nor the
                // host's, so its release excludes nothing.
                // [ORB-15573] Nor is a provider's content refusal.
                (None, Some(failure))
                    if failure.excludes_crew() && evidence.forge_hold.is_none() =>
                {
                    (
                        failure
                            .crew
                            .as_deref()
                            .or(default_crew.as_deref())
                            .map(|crew| vec![crew.to_string()])
                            .unwrap_or_default(),
                        CrewExclusionSource::LeafReleased,
                        format!(
                            "{task} was released ({}): {}",
                            failure.class.as_str(),
                            failure.reason
                        ),
                    )
                }
                _ => continue,
            };
            for crew in crews {
                if excluded.iter().any(|exclusion| exclusion.crew == crew) {
                    continue;
                }
                excluded.push(CrewExclusion {
                    crew,
                    source,
                    reason: reason.clone(),
                    until: None,
                });
            }
        }
        // [ORB-15791] Recomputed on every pass like a provider limit, so a
        // crew whose capacity backoff passed is runnable again in the same
        // window. A crew excluded for the window already keeps that.
        let now = Utc::now();
        let mut host_suppressed_by_busy_owner = false;
        let owner_busy_backoff = match owner_busy.verdict() {
            Some(OwnerBusyVerdict::Suppress(reason)) => {
                if host_suppressed.is_none() {
                    host_suppressed = Some(reason);
                    host_suppressed_by_busy_owner = true;
                }
                None
            }
            Some(OwnerBusyVerdict::Backoff(backoff)) if backoff.until > now => Some(backoff),
            _ => None,
        };
        for (crew, failures) in capacity {
            if excluded.iter().any(|exclusion| exclusion.crew == crew) {
                continue;
            }
            if let Some((until, reason)) = failures.backoff()
                && until > now
            {
                excluded.push(CrewExclusion {
                    crew,
                    source: CrewExclusionSource::ProviderUnavailable,
                    reason,
                    until: Some(until),
                });
            }
        }
        // [ORB-14697] Recomputed on every pass, so a crew whose reading
        // lapses is runnable again in the same window.
        let gate = self.provider_limit_gate(now);
        for crew in self.context.settings().crews().values() {
            if excluded.iter().any(|exclusion| exclusion.crew == crew.name) {
                continue;
            }
            if let Some(limit) = gate.limit_for(crew) {
                excluded.push(CrewExclusion {
                    crew: crew.name.clone(),
                    source: CrewExclusionSource::ProviderLimit,
                    reason: format!(
                        "{}; reading from host {}",
                        limit.describe(),
                        self.automation_machine_identity().unwrap_or("unregistered")
                    ),
                    until: Some(limit.until),
                });
            }
        }
        let timed = |crew: &str| {
            excluded
                .iter()
                .any(|exclusion| exclusion.crew == crew && exclusion.until.is_some())
        };
        // The crews the window can run once every timed exclusion lifts.
        // Review is not delivery admission, so a provider limit does not
        // keep the owner's reviewer from running here; a capacity backoff
        // does, until it lifts ([`PullCrewWindow::reviewer_unrunnable_reason`]).
        let reviewable: Option<Vec<String>> = preflight.as_ref().map(|preflight| {
            preflight
                .runnable
                .iter()
                .filter(|crew| {
                    !excluded
                        .iter()
                        .any(|exclusion| &exclusion.crew == *crew && exclusion.until.is_none())
                })
                .cloned()
                .collect()
        });
        // The run input is the authority for the restriction, as for an
        // owner drain: a name this host no longer configures fails the pass
        // rather than widening it.
        let input = self
            .stores()
            .jobs()
            .get_job_run(drain_run_id)?
            .and_then(|run| run.input)
            .unwrap_or(serde_json::Value::Null);
        let allowlist = self.crew_allowlist_from_input(&input)?;
        let unlimited = reviewable.as_ref().map(|crews| {
            crews
                .iter()
                .filter(|crew| !timed(crew))
                .cloned()
                .collect::<Vec<_>>()
        });
        let runnable = match &allowlist {
            None => unlimited,
            Some(allowlist) => Some(match &unlimited {
                Some(crews) => crews
                    .iter()
                    .filter(|crew| self.crew_allowlist_permits(allowlist, crew))
                    .cloned()
                    .collect(),
                None => allowlist
                    .names()
                    .into_iter()
                    .filter(|crew| !excluded.iter().any(|exclusion| exclusion.crew == *crew))
                    .map(ToOwned::to_owned)
                    .collect(),
            }),
        };
        // [ORB-17773] Per-crew weekly pressure from this host's own
        // readings, recomputed each pass like the limit gate above. Under
        // `static` nothing is reported, so admission and offer order are
        // unchanged.
        let usage_pressure = if self.provider_limit_policy().usage_pressure.applies() {
            let read = self.usage_pressure_read(now);
            let mut crews: Vec<_> = self
                .context
                .settings()
                .crews()
                .values()
                .filter(|crew| crew.enabled)
                .collect();
            crews.sort_by(|left, right| left.name.cmp(&right.name));
            crews
                .into_iter()
                .map(|crew| orbit_store::contracts::AdmissionCrewPressure {
                    crew: crew.name.clone(),
                    pressure_milli: pressure_milli(read.crew(crew).pressure),
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(PullCrewWindow {
            checked_at: preflight.as_ref().map(|preflight| preflight.checked_at),
            runnable,
            allowed: allowlist.map(|allowlist| {
                allowlist
                    .names()
                    .into_iter()
                    .map(ToOwned::to_owned)
                    .collect()
            }),
            reviewable,
            default_crew,
            excluded,
            auth_exclusions,
            host_suppressed,
            host_suppressed_by_busy_owner,
            owner_busy_backoff,
            usage_pressure,
        })
    }

    /// Whether `leaf`'s recorded step failure is provider authentication,
    /// not model capacity. Capacity is also stored as `provider_unavailable`
    /// evidence; only the authentication marker widens the exclusion.
    pub(crate) fn leaf_reported_authentication(&self, leaf: Option<&str>) -> bool {
        let Some(leaf) = leaf else {
            return false;
        };
        self.stores()
            .jobs()
            .get_job_run(leaf)
            .ok()
            .flatten()
            .is_some_and(|run| {
                run.steps.iter().any(|step| {
                    step.error_code.as_deref()
                        == Some(orbit_types::workflow::PROVIDER_UNAVAILABLE_ERROR_CODE)
                        || step.error_message.as_deref().is_some_and(|message| {
                            orbit_types::workflow::failure_classification_text(message)
                                .contains(orbit_types::workflow::PROVIDER_UNAVAILABLE_MARKER)
                        })
                })
            })
    }

    /// When `leaf` ended, or started when it records no end; `None` when its
    /// run is unknown here. A failed read is an error, not an unknown run.
    fn leaf_ended_at(&self, leaf: Option<&str>) -> Result<Option<DateTime<Utc>>, OrbitError> {
        let Some(leaf) = leaf else {
            return Ok(None);
        };
        Ok(self
            .stores()
            .jobs()
            .get_job_run(leaf)?
            .map(|run| run.finished_at.or(run.started_at).unwrap_or(run.created_at)))
    }

    /// `failed` plus every configured crew on the same provider account:
    /// whose provider parses to the same canonical provider and, [ORB-17288]
    /// as a crew that sets `env` is its own account, that shares `failed`'s
    /// account. A crew with `env` is alone on its account; one without
    /// shares the provider's default login with the others without. An
    /// unparsable or unknown crew stays a single name.
    fn crews_sharing_account(&self, failed: &str) -> Vec<String> {
        let registry = self.context.settings().crews();
        let Some((provider, account)) = registry.get(failed).and_then(|crew| {
            orbit_types::workflow::Provider::parse(&crew.assignment.provider)
                .ok()
                .map(|provider| (provider, crew.account()))
        }) else {
            return vec![failed.to_string()];
        };
        let mut names = registry
            .values()
            .filter(|crew| {
                orbit_types::workflow::Provider::parse(&crew.assignment.provider).ok()
                    == Some(provider)
                    && crew.account() == account
            })
            .map(|crew| crew.name.clone())
            .collect::<Vec<_>>();
        if !names.iter().any(|name| name == failed) {
            names.push(failed.to_string());
        }
        names
    }

    /// The pull admission a local run executes, when it is a claimed leaf.
    /// `None` for every other run, including runs on an owner checkout.
    pub fn pull_leaf_claim(
        &self,
        run_id: &str,
    ) -> Result<Option<PullLeafClaim>, orbit_common::OrbitError> {
        let Some(admission) = self.claimed_leaf_admission(run_id)? else {
            return Ok(None);
        };
        let claim = admission
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.claim.as_ref());
        let settlement_phase = phase_name(admission.phase);
        let settlement_refusal = RefusedPullSettlement::of(&admission);
        Ok(Some(PullLeafClaim {
            task_id: claim.map(|claim| claim.task_id.clone()),
            claim_id: claim.map(|claim| claim.claim_id.clone()),
            owner: admission.destination.selector.clone(),
            drain_run_id: admission.request.run_context.run_id.clone(),
            settlement_phase,
            failure_class: match &admission.settlement {
                Some(ClaimMutation::Fail(evidence) | ClaimMutation::Release(evidence)) => {
                    evidence.failure.as_ref().map(|failure| failure.class)
                }
                _ => None,
            },
            refusal: admission.refusal.clone(),
            guidance: phase_guidance(
                admission.phase,
                admission.refusal.as_deref(),
                settlement_refusal.as_ref(),
            ),
            settlement_refusal,
        }))
    }
}

/// Whether the owner answered a pull with a refusal, as opposed to a lost or
/// uncertain delivery.
///
/// Only an answer counts. A refusal from the owner's pre-admission ladder —
/// selector, capability, shape, version, ship mode, review policy, a stale
/// ship contract — commits nothing, so it is safe to close the request once
/// the owner also confirms it holds no receipt for it. A delivery miss, a lost
/// answer or a store failure says nothing about whether an earlier send of the
/// same request committed, so the request stays pending and is retried.
pub(crate) fn is_owner_refusal(error: &OrbitError) -> bool {
    matches!(
        error,
        OrbitError::RemoteTool {
            code: RemoteToolCode::ProtocolSkew
                | RemoteToolCode::InvalidInput
                | RemoteToolCode::CapabilityRefused
                | RemoteToolCode::CapabilityDenied
                | RemoteToolCode::PolicyDenied,
            ..
        } | OrbitError::InvalidInput(_)
            | OrbitError::ClaimRefused { .. }
            | OrbitError::ProtocolSkew(_)
            | OrbitError::CapabilityRefused(_)
            | OrbitError::CapabilityDenied(_)
            | OrbitError::PolicyDenied(_)
    )
}

/// Whether an error says the owner could not be reached or did not answer
/// cleanly: an unreachable or stale route, a lost or unknown outcome, an owner
/// that is unavailable, or an owner-side failure that is not a refusal.
///
/// Only these mean the next call to the same owner is likely to fail or hang
/// the same way, so a pass stops calling that owner after the first one. That
/// includes an owner that lost to its own lock-wait deadline (`lock_busy`):
/// it decided nothing, and the next call would queue behind the same holder,
/// so backing off until the next pass also eases the contention [ORB-15088].
/// A refusal is an answer, and a local error (a store read, a missing
/// binding) says nothing about the owner at all.
pub(crate) fn is_owner_transport_failure(error: &OrbitError) -> bool {
    match error {
        OrbitError::RemoteTool { .. } => !is_owner_refusal(error),
        OrbitError::UnreachableDestination(_)
        | OrbitError::OutcomeUnknown { .. }
        | OrbitError::OwnerUnavailable(_)
        | OrbitError::OwnerNegotiation(_)
        | OrbitError::StaleRoute(_)
        | OrbitError::UnhealthyCheckout(_) => true,
        _ => false,
    }
}
