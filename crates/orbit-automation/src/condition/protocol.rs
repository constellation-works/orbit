//! The firing protocol (`docs/design/condition-triggers/1_scope.md` §3): load
//! the consumer at generation `g`, evaluate, commit `g+1` with each new edge
//! as an intent, then admit each intent under its action key and commit the
//! acknowledgement. A losing evaluator writes nothing; a crash between any
//! two steps is finished by the next pass under the same key, so an edge
//! admits its action exactly once.

use chrono::{DateTime, Utc};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::{
    CONDITION_ADMISSION_ATTEMPTS, ConditionFire, ConditionFireState, ConditionState,
    ConditionTrigger,
};
use serde::Serialize;

use super::firing::{ConditionObservation, PassSettings, advance_pass};
use crate::{AutomationError, DeferredReason};

/// Owner-side facts and the keyed actions.
pub trait ConditionHost {
    /// Evaluate the subjects. `state` is the committed state, for tracked
    /// subjects, the lap continuation and `self`.
    fn observe(
        &self,
        state: &ConditionState,
        now: DateTime<Utc>,
    ) -> Result<ConditionObservation, AutomationError>;

    /// The task or run already admitted under the fire's action key.
    fn lookup(&self, fire: &ConditionFire) -> Result<Option<String>, AutomationError>;

    /// Why the fire's action must wait this pass (an open instance under
    /// `skip_if_open`). A waiting intent keeps its key and spends no attempt.
    fn admission_deferral(&self, _fire: &ConditionFire) -> Result<Option<String>, AutomationError> {
        Ok(None)
    }

    /// Admit the fire's action under its action key, with its captured
    /// subject. Must be idempotent per key.
    fn admit(&self, fire: &ConditionFire) -> Result<String, AutomationError>;
}

/// One evaluation request.
#[derive(Debug, Clone, Copy)]
pub struct ConditionEvaluation<'a> {
    pub consumer: &'a str,
    pub epoch: &'a str,
    pub trigger: &'a ConditionTrigger,
    pub enabled: bool,
    /// Why this host may not fire, when it is not the owner (`owned_elsewhere`,
    /// `ownership_unresolved`, `replica_checkout`).
    pub refusal: Option<&'static str>,
    pub dry_run: bool,
    pub now: DateTime<Utc>,
    pub cadence_seconds: u64,
}

/// One admitted (or attempted) action, for reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConditionAction {
    pub fire_id: String,
    pub subject: String,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The pass result.
#[derive(Debug, Clone)]
pub struct ConditionDiagnostic {
    pub reason: String,
    pub state: Option<ConditionState>,
    /// Actions this pass admitted or retried.
    pub actions: Vec<ConditionAction>,
    /// Dry run: the subjects that would fire.
    pub would_fire: Vec<String>,
}

impl ConditionDiagnostic {
    fn with(reason: &str, state: Option<ConditionState>) -> Self {
        Self {
            reason: reason.to_string(),
            state,
            actions: Vec::new(),
            would_fire: Vec::new(),
        }
    }
}

fn commit(
    store: &dyn AutomationStoreBackend,
    previous: &ConditionState,
    mut next: ConditionState,
) -> Result<ConditionState, AutomationError> {
    next.generation = previous
        .generation
        .checked_add(1)
        .ok_or(AutomationError::Deferred(
            DeferredReason::GenerationExhausted,
        ))?;
    if !store.condition_commit(previous, &next)? {
        return Err(AutomationError::Deferred(
            DeferredReason::ConcurrentEvaluation,
        ));
    }
    Ok(next)
}

/// Run one pass for one condition consumer.
pub fn evaluate_condition(
    store: &dyn AutomationStoreBackend,
    host: &dyn ConditionHost,
    request: ConditionEvaluation<'_>,
) -> Result<ConditionDiagnostic, AutomationError> {
    // Missing state is a first pass; unreadable state fails closed here and
    // never becomes a fresh baseline.
    let stored = store.condition_state(request.consumer)?;

    // A follower, a replica or an unresolved owner never fires and never
    // admits a pending intent: the owner finishes its own.
    if let Some(refusal) = request.refusal {
        return Ok(ConditionDiagnostic::with(refusal, stored));
    }

    if !request.enabled {
        let Some(state) = stored else {
            return Ok(ConditionDiagnostic::with("disabled", None));
        };
        if !state.enabled || request.dry_run {
            return Ok(ConditionDiagnostic::with("disabled", Some(state)));
        }
        // Seeing the disable makes the next enable a baseline (§3).
        let mut next = state.clone();
        next.enabled = false;
        next.lap_after = None;
        for subject in next.subjects.values_mut() {
            subject.true_since = None;
            subject.deferred = None;
            subject.deferred_since = None;
        }
        let state = commit(store, &state, next)?;
        return Ok(ConditionDiagnostic::with("disabled", Some(state)));
    }

    let (state, mut actions) = match stored {
        Some(state) if !request.dry_run => settle(store, host, state, request.now)?,
        Some(state) => (state, Vec::new()),
        None => {
            let fresh = ConditionState::new(request.consumer, request.epoch);
            if !request.dry_run && !store.condition_initialize(&fresh)? {
                return Err(AutomationError::Deferred(
                    DeferredReason::ConcurrentEvaluation,
                ));
            }
            (fresh, Vec::new())
        }
    };

    if let Some(stall) = &state.stall {
        let mut diagnostic = ConditionDiagnostic::with(&stall.reason, Some(state.clone()));
        diagnostic.actions = actions;
        return Ok(diagnostic);
    }

    let observation = host.observe(&state, request.now)?;
    let outcome = advance_pass(
        &state,
        &observation,
        PassSettings {
            trigger: request.trigger,
            epoch: request.epoch,
            now: request.now,
            cadence_seconds: request.cadence_seconds,
        },
    )?;

    if request.dry_run {
        let would_fire = outcome
            .fired
            .iter()
            .filter_map(|id| outcome.next.fire(id).map(|fire| fire.subject.clone()))
            .collect();
        return Ok(ConditionDiagnostic {
            reason: outcome.reason.to_string(),
            state: Some(outcome.next),
            actions,
            would_fire,
        });
    }

    let committed = commit(store, &state, outcome.next)?;
    let (state, admitted) = if outcome.fired.is_empty() {
        (committed, Vec::new())
    } else {
        settle(store, host, committed, request.now)?
    };
    actions.extend(admitted);
    Ok(ConditionDiagnostic {
        reason: outcome.reason.to_string(),
        state: Some(state),
        actions,
        would_fire: Vec::new(),
    })
}

/// Admit every recorded intent under its key and commit what happened. An
/// action the key already resolves to is acknowledged, never admitted again.
fn settle(
    store: &dyn AutomationStoreBackend,
    host: &dyn ConditionHost,
    state: ConditionState,
    now: DateTime<Utc>,
) -> Result<(ConditionState, Vec<ConditionAction>), AutomationError> {
    if !state
        .fires
        .iter()
        .any(|fire| fire.state == ConditionFireState::Intent)
    {
        return Ok((state, Vec::new()));
    }
    let mut next = state.clone();
    let mut actions = Vec::new();
    for fire in next
        .fires
        .iter_mut()
        .filter(|fire| fire.state == ConditionFireState::Intent)
    {
        let resolved = match host.lookup(fire)? {
            Some(id) => Ok(id),
            None => match host.admission_deferral(fire)? {
                Some(reason) => {
                    fire.detail = Some(format!("waiting: {reason}"));
                    actions.push(ConditionAction {
                        fire_id: fire.fire_id.clone(),
                        subject: fire.subject.clone(),
                        state: fire.state.as_str(),
                        action_id: None,
                        detail: fire.detail.clone(),
                    });
                    continue;
                }
                None => host.admit(fire),
            },
        };
        match resolved {
            Ok(id) => {
                fire.state = ConditionFireState::Acknowledged;
                fire.action_id = Some(id);
                fire.acknowledged_at = Some(now);
                fire.detail = None;
            }
            Err(error) => {
                fire.attempts = fire.attempts.saturating_add(1);
                fire.detail = Some(error.to_string());
                if fire.attempts >= CONDITION_ADMISSION_ATTEMPTS {
                    fire.state = ConditionFireState::Failed;
                }
            }
        }
        actions.push(ConditionAction {
            fire_id: fire.fire_id.clone(),
            subject: fire.subject.clone(),
            state: fire.state.as_str(),
            action_id: fire.action_id.clone(),
            detail: fire.detail.clone(),
        });
    }
    if next == state {
        return Ok((state, actions));
    }
    let state = commit(store, &state, next)?;
    Ok((state, actions))
}
