//! Keep delivery admission off crews whose provider is near or at its usage
//! limit on this host [ORB-14697].
//!
//! The host's provider-limit store holds the latest reading of each usage
//! window (`runtime::engine::invocation`). A reading gates the crews of its
//! provider — or, when it names a model, only the crews on that model — while
//! it is live and either exhausted or used at or above the provider's
//! `workflow.provider_limit_*` threshold. It is live until the reset it
//! reported; one with no reset lives for its window's length, or an hour when
//! that is unknown, and an exhausted error reading with no reset for the first
//! backoff a usage-limit hold uses. Nothing has to probe the provider again:
//! the next admission pass after that simply draws the crew again.
//!
//! Limited crews are removed from a task's draw before it is made. A task
//! drawn from a pool or the default falls back to the unlimited members of
//! its complexity pool, never to the workspace default. A task whose own
//! crew is its explicit choice waits under `wait`, and under `pool` is drawn
//! from that pool too. When nothing unlimited is left the task waits, and the
//! local drain's backlog snapshot reports it as `provider_limit`. Only
//! delivery admission is gated; an explicit run-input crew is the operator's
//! call and records the limit it ran against instead.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_config::{ProviderLimitExplicitCrews, ProviderLimitPolicy};
use orbit_types::identity::Crew;
use orbit_types::task::Task;
use orbit_types::telemetry::{ProviderLimitObservation, ProviderLimitSource};
use orbit_types::workflow::{ProviderFailureClass, ProviderFailureHold};

use super::provider_limit_ledger::PARTIAL_NOTE;
use crate::OrbitRuntime;
use crate::application::job::crew_pools::{CapturedCrewPools, CrewCandidate};
use crate::runtime::run_input::non_empty;
use crate::runtime::task::provider_hold::{base_backoff, same_provider};

/// How long a reading that reported neither a reset nor its window's length
/// counts.
const UNKNOWN_WINDOW_LIFETIME: Duration = Duration::minutes(60);

/// One live reading that keeps its provider's crews out of admission.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ProviderLimit {
    provider: String,
    model: Option<String>,
    window: Option<String>,
    used_percent: Option<f64>,
    /// Whether the reading undercounts (a ledger budget missing costs).
    partial: bool,
    threshold: u8,
    /// When the reading stops counting.
    pub(crate) until: DateTime<Utc>,
}

impl ProviderLimit {
    /// The limit `observation` places at `now`, if it is live and gates.
    fn of(
        observation: &ProviderLimitObservation,
        policy: &ProviderLimitPolicy,
        now: DateTime<Utc>,
    ) -> Option<Self> {
        if !observation.gating {
            return None;
        }
        let until = reading_lifetime(observation);
        if until <= now {
            return None;
        }
        let threshold = policy.threshold(&observation.provider);
        reading_at_limit(observation, threshold).then(|| Self {
            provider: observation.provider.clone(),
            model: observation.model.clone(),
            window: observation.window.clone(),
            used_percent: observation.used_percent,
            partial: observation.partial,
            threshold,
            until,
        })
    }

    /// Whether the limit applies to `crew`: same provider, and the limit's
    /// model, when it names one, is the crew's.
    fn covers(&self, crew: &Crew) -> bool {
        reading_covers(&self.provider, self.model.as_deref(), crew)
    }

    /// `<provider> <window> at <used>% (limit <threshold>%) until <reset>`.
    pub(crate) fn describe(&self) -> String {
        let window = self
            .window
            .clone()
            .or_else(|| self.model.as_ref().map(|model| format!("{model} window")))
            .unwrap_or_else(|| "usage window".to_string());
        let usage = self
            .used_percent
            .map_or_else(|| "exhausted".to_string(), |used| format!("at {used}%"));
        let partial = if self.partial {
            format!(" ({PARTIAL_NOTE})")
        } else {
            String::new()
        };
        format!(
            "{} {window} {usage}{partial} (limit {}%) until {}",
            self.provider,
            self.threshold,
            self.until.to_rfc3339()
        )
    }
}

/// When `observation` stops counting: the reset it reported; else, for an
/// exhausted error reading, the first backoff a usage-limit hold uses; else
/// its window's length, or an hour when that is unknown.
pub(super) fn reading_lifetime(observation: &ProviderLimitObservation) -> DateTime<Utc> {
    match (observation.resets_at, observation.window_minutes) {
        (Some(resets_at), _) => resets_at,
        (None, _) if observation.exhausted && observation.source == ProviderLimitSource::Error => {
            observation.observed_at + base_backoff(ProviderFailureClass::Limit)
        }
        (None, Some(minutes)) => observation.observed_at + Duration::minutes(minutes.into()),
        (None, None) => observation.observed_at + UNKNOWN_WINDOW_LIFETIME,
    }
}

/// Whether `observation` is exhausted or used at or above `threshold`. A
/// threshold of 100 gates only on exhaustion.
pub(super) fn reading_at_limit(observation: &ProviderLimitObservation, threshold: u8) -> bool {
    observation.exhausted
        || (threshold < 100
            && observation
                .used_percent
                .is_some_and(|used| used >= f64::from(threshold)))
}

/// Whether a reading of `provider`, scoped to `model` when it names one,
/// applies to `crew`.
pub(super) fn reading_covers(provider: &str, model: Option<&str>, crew: &Crew) -> bool {
    same_provider(Some(&crew.assignment.provider), Some(provider))
        && model.is_none_or(|model| {
            crew.assignment
                .model
                .to_ascii_lowercase()
                .contains(&model.to_ascii_lowercase())
        })
}

/// The host's live provider limits at one admission.
#[derive(Debug, Default)]
pub(crate) struct ProviderLimitGate {
    limits: Vec<ProviderLimit>,
    explicit_crews: ProviderLimitExplicitCrews,
}

impl ProviderLimitGate {
    pub(crate) fn new(
        observations: &[ProviderLimitObservation],
        policy: &ProviderLimitPolicy,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            limits: observations
                .iter()
                .filter_map(|observation| ProviderLimit::of(observation, policy, now))
                .collect(),
            explicit_crews: policy.explicit_crews,
        }
    }

    /// Whether no crew is limited.
    pub(crate) fn is_empty(&self) -> bool {
        self.limits.is_empty()
    }

    /// The limit holding `crew` back, the one lasting longest when several
    /// do.
    pub(crate) fn limit_for(&self, crew: &Crew) -> Option<&ProviderLimit> {
        self.limits
            .iter()
            .filter(|limit| limit.covers(crew))
            .max_by_key(|limit| limit.until)
    }

    /// `<limit>[, <limit>...]; crews <list> skipped` for the limited `crews`.
    fn skipped<'a>(&self, crews: impl IntoIterator<Item = &'a Crew>) -> String {
        let mut names = BTreeSet::new();
        let mut limits = Vec::new();
        for crew in crews {
            if let Some(limit) = self.limit_for(crew) {
                names.insert(crew.name.as_str());
                let limit = limit.describe();
                if !limits.contains(&limit) {
                    limits.push(limit);
                }
            }
        }
        format!(
            "{}; crews {} skipped",
            limits.join(", "),
            names.into_iter().collect::<Vec<_>>().join(", ")
        )
    }
}

/// What the host's provider limits leave of a task's draw.
pub(crate) enum LimitedDraw {
    /// No crew the task would be drawn from is limited.
    Unlimited,
    /// Draw from these unlimited crews, with the provenance naming the limit.
    Narrowed(Vec<CrewCandidate>, String),
    /// Every crew the task may run as is limited; the detail says why.
    Wait(String),
}

impl OrbitRuntime {
    /// The host's live provider limits at `now`. An unreadable store is
    /// logged and gates nothing: the limits are advisory, and the provider
    /// still refuses a run past its real limit.
    pub(crate) fn provider_limit_gate(&self, now: DateTime<Utc>) -> ProviderLimitGate {
        match self.provider_limit_observations(now) {
            Ok(observations) => {
                ProviderLimitGate::new(&observations, self.context.settings().provider_limit(), now)
            }
            Err(error) => {
                tracing::warn!("could not read this host's provider usage limits: {error}");
                ProviderLimitGate::default()
            }
        }
    }

    /// Whether `task`'s crew is its own choice — an explicit assignment or a
    /// legacy pin — rather than a pool's or the default's.
    pub(crate) fn explicit_task_crew(&self, task: &Task) -> Result<bool, OrbitError> {
        let source = self.task_crew_source(task)?;
        if source
            .as_deref()
            .is_some_and(|source| source == "default" || source.starts_with("pool:"))
        {
            return Ok(false);
        }
        Ok(task.crew.as_deref().and_then(non_empty).is_some())
    }

    /// What `gate` leaves of `task`'s draw over `candidates`, which `source`
    /// produced and a standing `hold` has already narrowed.
    pub(crate) fn provider_limited_candidates(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
        hold: Option<&ProviderFailureHold>,
        gate: &ProviderLimitGate,
        candidates: &[CrewCandidate],
        source: &str,
    ) -> Result<LimitedDraw, OrbitError> {
        if gate.is_empty() {
            return Ok(LimitedDraw::Unlimited);
        }
        // A hold that excluded every crew left the draw as it was, so its
        // crews are still filtered here.
        let drawable = |candidate: &CrewCandidate| {
            candidate.weight > 0 && !hold.is_some_and(|hold| hold.excludes(&candidate.crew.name))
        };
        let limited = |candidate: &CrewCandidate| gate.limit_for(&candidate.crew).is_some();
        let mut skipped = candidates
            .iter()
            .filter(|candidate| drawable(candidate) && limited(candidate))
            .map(|candidate| candidate.crew.clone())
            .collect::<Vec<_>>();
        if skipped.is_empty() {
            return Ok(LimitedDraw::Unlimited);
        }
        let unlimited = candidates
            .iter()
            .filter(|candidate| drawable(candidate) && !limited(candidate))
            .cloned()
            .collect::<Vec<_>>();
        if !unlimited.is_empty() {
            return Ok(LimitedDraw::Narrowed(
                unlimited,
                format!("{source}; provider limit: {}", gate.skipped(&skipped)),
            ));
        }
        // No default fallback: the default is usually the most expensive
        // crew and often shares a provider with the pool.
        if (gate.explicit_crews == ProviderLimitExplicitCrews::Pool
            || !self.explicit_task_crew(task)?)
            && let Some((pool, pool_source)) =
                self.complexity_pool_candidates(task.complexity, pools)?
        {
            let permitted = pool.into_iter().filter(drawable).collect::<Vec<_>>();
            skipped.extend(
                permitted
                    .iter()
                    .filter(|candidate| limited(candidate))
                    .map(|candidate| candidate.crew.clone()),
            );
            let unlimited = permitted
                .into_iter()
                .filter(|candidate| !limited(candidate))
                .collect::<Vec<_>>();
            if !unlimited.is_empty() {
                return Ok(LimitedDraw::Narrowed(
                    unlimited,
                    format!("{pool_source}; provider limit: {}", gate.skipped(&skipped)),
                ));
            }
        }
        Ok(LimitedDraw::Wait(gate.skipped(&skipped)))
    }

    /// The crews `task` is drawn from, honouring a standing provider hold and
    /// this host's provider limits when they leave any crew. When they leave
    /// none, [`Self::provider_backoff_deferral`] or
    /// [`Self::provider_limit_deferral`] reports why, and the draw stays as
    /// it was, so an operator's explicit ship still runs.
    pub(crate) fn admissible_crew_candidates(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
    ) -> Result<(Vec<CrewCandidate>, String), OrbitError> {
        let (candidates, source, hold) = self.held_crew_candidates(task, pools)?;
        let gate = self.provider_limit_gate(Utc::now());
        Ok(
            match self.provider_limited_candidates(
                task,
                pools,
                hold.as_ref(),
                &gate,
                &candidates,
                &source,
            )? {
                LimitedDraw::Narrowed(candidates, source) => (candidates, source),
                LimitedDraw::Unlimited | LimitedDraw::Wait(_) => (candidates, source),
            },
        )
    }

    /// Why `task` waits out a provider limit at this admission: `Some` when
    /// every crew it may run as is limited, `None` when it may be drawn.
    pub(crate) fn provider_limit_deferral(
        &self,
        task: &Task,
        pools: &CapturedCrewPools,
    ) -> Result<Option<String>, OrbitError> {
        let gate = self.provider_limit_gate(Utc::now());
        if gate.is_empty() {
            return Ok(None);
        }
        let (candidates, source, hold) = self.held_crew_candidates(task, pools)?;
        Ok(
            match self.provider_limited_candidates(
                task,
                pools,
                hold.as_ref(),
                &gate,
                &candidates,
                &source,
            )? {
                LimitedDraw::Wait(detail) => Some(detail),
                LimitedDraw::Unlimited | LimitedDraw::Narrowed(..) => None,
            },
        )
    }
}
