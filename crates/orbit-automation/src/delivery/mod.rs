//! One bounded delivery evaluator shared by task and job consumers.

use crate::AutomationError;
use crate::checkpoint::{commit, diagnostic};
use chrono::{DateTime, Utc};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::*;
use sha2::{Digest, Sha256};

pub mod evidence;
mod observe;
pub mod recovery;
#[cfg(test)]
mod tests;

/// Authority/source owner, implemented by Core. It never decides coverage rules.
pub trait DeliveryHost {
    fn admission_deferral(&self) -> Result<Option<String>, AutomationError> {
        Ok(None)
    }

    fn head(&self, branch: &str) -> Result<(String, SourceRevision), AutomationError>;

    fn observe(&self, branch: &str, state: &AutomationState)
    -> Result<SourcePage, AutomationError>;

    /// Canonical action admission must resolve the same durable key on replay.
    fn admit(&self, attempt: &BatchAttempt) -> Result<String, AutomationError>;

    fn outcome(&self, attempt: &BatchAttempt) -> Result<ActionOutcome, AutomationError>;
}

/// Core's authoritative observation of an admitted task/job.
pub enum ActionOutcome {
    Pending,
    Evidence(evidence::EvidenceFacts),
    /// Only returned after proving the action stopped. Unknown liveness is Pending.
    Failed {
        retryable: bool,
        reason: String,
    },
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn input_digest(batch: &CoverageBatch) -> Result<String, AutomationError> {
    serde_json::to_vec(batch)
        .map(|bytes| digest(&bytes))
        .map_err(|e| AutomationError::Evidence(e.to_string()))
}

pub fn definition_epoch<T: serde::Serialize>(definition: &T) -> Result<String, AutomationError> {
    serde_json::to_vec(definition)
        .map(|bytes| digest(&bytes))
        .map_err(|e| AutomationError::Evidence(e.to_string()))
}

/// An edited definition pauses new admission until it is restored. Hosts that
/// layer their own reasons over this one report it ahead of theirs.
pub const DEFINITION_CHANGED: &str = "definition_changed";

/// Inputs supplied by the existing sweep clock.
pub struct Evaluation<'a> {
    pub consumer: &'a str,
    pub epoch: &'a str,
    pub trigger: &'a DeliveryTrigger,
    pub enabled: bool,
    pub dry_run: bool,
    pub now: DateTime<Utc>,
}

/// Evaluate explicit configuration without another scheduler or ticking loop.
pub fn evaluate(
    store: &dyn AutomationStoreBackend,
    host: &dyn DeliveryHost,
    request: Evaluation<'_>,
) -> Result<AutomationDiagnostic, AutomationError> {
    let Evaluation {
        consumer,
        epoch,
        trigger,
        enabled,
        dry_run,
        now,
    } = request;

    trigger.validate().map_err(orbit_common::OrbitError::from)?;

    // The first evaluation pins a baseline at the branch head; nothing before it is debt.
    let mut state = match store.automation_state(consumer)? {
        Some(state) => state,
        None => {
            // Disabled definitions never pin a baseline. Preview must match the
            // real pass here: probing git would invent `would_baseline` or an
            // `evidence_unavailable` error the scheduled tick never sees.
            if !enabled {
                return diagnostic(store, consumer, "disabled", None);
            }

            let (repository, head) = host.head(&trigger.branch)?;
            let state = AutomationState {
                members: None,
                consumer: consumer.into(),
                epoch: epoch.into(),
                trigger: Some(trigger.clone()),
                repository,
                branch: trigger.branch.clone(),
                generation: 0,
                baseline: head.clone(),
                observed: head.clone(),
                covered: head,
                pending_commits: vec![],
                pending: vec![],
                waived: vec![],
                excluded: vec![],
                unresolved: Default::default(),
                associations: Default::default(),
                active: None,
            };

            if !dry_run {
                store.automation_initialize(&state)?;
            }

            return diagnostic(
                store,
                consumer,
                if dry_run {
                    "would_baseline"
                } else {
                    "baselined"
                },
                Some(state),
            );
        }
    };

    // Reconcile admitted work even when disabled or a definition was edited.
    if !dry_run
        && state
            .active
            .as_ref()
            .is_some_and(|attempt| attempt.action_id.is_some())
    {
        state = reconcile(store, host, state, now)?;
    }

    if state.epoch != epoch || state.branch != trigger.branch {
        return diagnostic(store, consumer, DEFINITION_CHANGED, Some(state));
    }

    if !enabled {
        return diagnostic(store, consumer, "disabled", Some(state));
    }

    // A claim that never reached admission resumes here, subject to its retry budget.
    if let Some(active) = &state.active
        && active.action_id.is_none()
        && active.state == BatchState::Claimed
        && !dry_run
    {
        if active.attempt > 1 && now > active.deadline() {
            let mut next = state.clone();
            if let Some(attempt) = &mut next.active {
                attempt.state = BatchState::Exhausted;
                attempt.reason = Some("retry_deadline_expired".into());
            }

            state = commit(store, &state, next, None)?;

            return diagnostic(store, consumer, "retry_deadline_expired", Some(state));
        }

        if active.retry_after.is_some_and(|at| now < at) {
            return diagnostic(store, consumer, "retry_backoff", Some(state));
        }

        state = admit_active(store, host, &state)?;
    }

    // Proven-covered prefixes are not debt. Retire them before backpressure so
    // an already-stalled excluded-only consumer can observe again.
    state = retire_covered_prefix(store, state, dry_run)?;

    // Backpressure pauses observation, never admission of already retained debt.
    if state.pending.len() < 950 && state.pending_commits.len() <= 4800 {
        let page = host.observe(&trigger.branch, &state)?;
        let next = observe::apply(&state, page, trigger.coverage, now)?;
        state = if dry_run {
            next
        } else {
            commit(store, &state, next, None)?
        };
        state = retire_covered_prefix(store, state, dry_run)?;
    }

    if let Some(active) = &state.active {
        let reason = match active.state {
            BatchState::Failed => "batch_failed",
            BatchState::Exhausted => "needs_attention",
            _ => "batch_pending",
        };
        return diagnostic(store, consumer, reason, Some(state));
    }

    let due_count = state.pending.len() >= trigger.threshold;
    let due_age = state.pending.first().is_some_and(|d| {
        now.signed_duration_since(d.landed_at).num_minutes() >= i64::from(trigger.max_wait_minutes)
    });

    if !due_count && !due_age {
        return diagnostic(
            store,
            consumer,
            if state.unresolved.is_empty() {
                "not_due"
            } else {
                "evidence_unavailable"
            },
            Some(state),
        );
    }

    if let Some(reason) = host.admission_deferral()? {
        return diagnostic(store, consumer, &reason, Some(state));
    }

    if dry_run {
        return diagnostic(
            store,
            consumer,
            if due_count {
                "threshold_reached"
            } else {
                "max_wait_reached"
            },
            Some(state),
        );
    }

    // Freeze an oldest prefix. Unattributed neighbors remain explicit obligations.
    let deliveries = state
        .pending
        .iter()
        .take(trigger.max_items)
        .cloned()
        .collect::<Vec<_>>();

    let through = if state.pending.len() > deliveries.len() {
        deliveries
            .last()
            .ok_or_else(|| AutomationError::Deferred("empty_batch".into()))?
            .after
            .clone()
    } else {
        state.observed.clone()
    };

    let end = state
        .pending_commits
        .iter()
        .position(|sha| sha == &through.commit)
        .ok_or_else(|| AutomationError::Deferred("delivery_boundary_missing".into()))?
        + 1;

    let commits = state.pending_commits[..end].to_vec();

    if deliveries
        .iter()
        .any(|delivery| !delivery.commits.iter().all(|sha| commits.contains(sha)))
    {
        return Err(AutomationError::Deferred(
            "delivery_crosses_boundary".into(),
        ));
    }

    // Excluded landings inside the range travel with the batch as readable
    // context; they are not obligations and the evidence never lists them.
    let exclusions = state
        .excluded
        .iter()
        .filter(|excluded| {
            excluded
                .delivery
                .commits
                .iter()
                .all(|sha| commits.contains(sha))
        })
        .cloned()
        .collect::<Vec<_>>();

    let mut batch = CoverageBatch {
        schema_version: 1,
        id: String::new(),
        consumer: consumer.into(),
        epoch: epoch.into(),
        repository: state.repository.clone(),
        branch: state.branch.clone(),
        coverage: trigger.coverage,
        from_exclusive: state.covered.clone(),
        through_inclusive: through,
        commits,
        deliveries,
        exclusions,
        created_at: now,
        max_attempts: trigger.retries + 1,
        retry_until: now + chrono::Duration::hours(24),
    };

    // The identity digest covers the batch before it carries an id; the frozen input
    // digest then covers the identified batch that evidence must be checked against.
    batch.id = input_digest(&batch)?;
    let frozen_input_digest = input_digest(&batch)?;

    if serde_json::to_vec(&batch)
        .map_err(|e| AutomationError::Evidence(e.to_string()))?
        .len()
        > 1_048_576
    {
        return diagnostic(store, consumer, "batch_too_large", Some(state));
    }

    let attempt = BatchAttempt {
        action_key: format!("automation:{}:1", batch.id),
        batch,
        input_digest: frozen_input_digest,
        attempt: 1,
        action_id: None,
        state: BatchState::Claimed,
        reason: None,
        retry_after: None,
        reissue: None,
    };

    let mut next = state.clone();
    next.active = Some(attempt);
    state = commit(store, &state, next, None)?;

    state = admit_active(store, host, &state)?;

    diagnostic(
        store,
        consumer,
        if due_count {
            "threshold_reached"
        } else {
            "max_wait_reached"
        },
        Some(state),
    )
}

/// Hand the claimed attempt to Core and record the durable action id it resolved.
fn admit_active(
    store: &dyn AutomationStoreBackend,
    host: &dyn DeliveryHost,
    state: &AutomationState,
) -> Result<AutomationState, AutomationError> {
    let active = state
        .active
        .as_ref()
        .ok_or_else(|| AutomationError::Deferred("missing_claim".into()))?;
    let id = host.admit(active)?;

    let mut next = state.clone();
    if let Some(attempt) = &mut next.active {
        attempt.action_id = Some(id);
        attempt.state = BatchState::Admitted;
    }

    commit(store, state, next, None)
}

fn retire_covered_prefix(
    store: &dyn AutomationStoreBackend,
    state: AutomationState,
    dry_run: bool,
) -> Result<AutomationState, AutomationError> {
    let Some(next) = observe::retire_excluded_prefix(&state) else {
        return Ok(state);
    };
    if dry_run {
        Ok(next)
    } else {
        commit(store, &state, next, None)
    }
}

fn reconcile(
    store: &dyn AutomationStoreBackend,
    host: &dyn DeliveryHost,
    state: AutomationState,
    now: DateTime<Utc>,
) -> Result<AutomationState, AutomationError> {
    let Some(active) = &state.active else {
        return Ok(state);
    };

    if matches!(
        active.state,
        BatchState::Exhausted | BatchState::Failed | BatchState::Waived
    ) {
        return Ok(state);
    }

    match host.outcome(active)? {
        ActionOutcome::Pending => Ok(state),
        ActionOutcome::Evidence(facts) => {
            let receipt = match evidence::validate(active, &facts, now) {
                Ok(receipt) => receipt,
                Err(error) => {
                    let mut next = state.clone();
                    if let Some(attempt) = &mut next.active {
                        attempt.reason = Some(error.to_string());
                    }

                    return commit(store, &state, next, None);
                }
            };

            // Accepted evidence retires the batch: its commits and every fact keyed to
            // them leave the pending window, and the covered cursor advances.
            let mut next = state.clone();
            next.covered = active.batch.through_inclusive.clone();
            next.pending_commits = next.pending_commits[active.batch.commits.len()..].to_vec();
            next.waived.retain(|delivery| {
                !delivery
                    .commits
                    .iter()
                    .all(|sha| active.batch.commits.contains(sha))
            });
            next.pending.retain(|delivery| {
                !delivery
                    .commits
                    .iter()
                    .all(|sha| active.batch.commits.contains(sha))
            });
            next.excluded.retain(|excluded| {
                !excluded
                    .delivery
                    .commits
                    .iter()
                    .all(|sha| active.batch.commits.contains(sha))
            });
            next.unresolved
                .retain(|sha, _| !active.batch.commits.contains(sha));
            next.associations
                .retain(|sha, _| !active.batch.commits.contains(sha));
            next.active = None;

            commit(store, &state, next, Some(&receipt))
        }
        ActionOutcome::Failed { retryable, reason } => {
            let mut next = state.clone();
            if let Some(attempt) = &mut next.active {
                attempt.reason = Some(reason);

                let retry_budget_remains = retryable
                    && attempt.attempt < attempt.batch.max_attempts
                    && now < attempt.batch.retry_until;

                if retry_budget_remains {
                    attempt.attempt += 1;
                    attempt.retry_after = Some(now + chrono::Duration::minutes(5));
                    attempt.action_key =
                        format!("automation:{}:{}", attempt.batch.id, attempt.attempt);
                    attempt.action_id = None;
                    attempt.state = BatchState::Claimed;
                } else {
                    attempt.state = if retryable {
                        BatchState::Exhausted
                    } else {
                        BatchState::Failed
                    };
                }
            }

            commit(store, &state, next, None)
        }
    }
}

/// Waive only settled failed/exhausted work; retain its code as a coverage gap.
pub fn waive(
    store: &dyn AutomationStoreBackend,
    consumer: &str,
    request: &WaiveBatchRequest,
    by: &str,
    now: DateTime<Utc>,
) -> Result<(), AutomationError> {
    if request.reason.trim().is_empty() || by.trim().is_empty() {
        return Err(AutomationError::Evidence(
            "waiver requires a reason and actor".into(),
        ));
    }

    // Waiving is idempotent: a batch already recorded as waived needs no second write.
    if store
        .automation_waivers(consumer, 100)?
        .iter()
        .any(|w| w.batch_id == request.batch_id)
    {
        return Ok(());
    }

    let previous = store
        .automation_state(consumer)?
        .ok_or_else(|| AutomationError::Evidence("consumer not found".into()))?;

    let active = previous
        .active
        .as_ref()
        .ok_or_else(|| AutomationError::Evidence("no active batch".into()))?;

    if active.batch.id != request.batch_id
        || !matches!(active.state, BatchState::Failed | BatchState::Exhausted)
    {
        return Err(AutomationError::Evidence(
            "only the current settled failed or exhausted batch can be waived".into(),
        ));
    }

    let mut next = previous.clone();
    next.generation = previous
        .generation
        .checked_add(1)
        .ok_or_else(|| AutomationError::Deferred("generation_exhausted".into()))?;
    next.active = None;
    next.pending.retain(|delivery| {
        !active
            .batch
            .deliveries
            .iter()
            .any(|member| member.key == delivery.key)
    });
    next.waived.extend(active.batch.deliveries.clone());

    let waiver = BatchWaiver {
        batch_id: request.batch_id.clone(),
        reason: request.reason.clone(),
        by: by.into(),
        at: now,
    };

    if !store.automation_waive(&previous, &next, &waiver)? {
        return Err(AutomationError::Deferred("concurrent_evaluation".into()));
    }

    Ok(())
}
