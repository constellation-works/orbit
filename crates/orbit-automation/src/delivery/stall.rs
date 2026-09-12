//! What the evaluator does with a deferred reason that does not clear itself.
//!
//! Most deferrals are ordinary backpressure — `source_backpressure`,
//! `concurrent_evaluation`, an exhausted per-pass budget, a superseded claim:
//! the next tick retries and the consumer catches up, and nothing is written.
//! A few reasons instead describe a source or identity fact that will read
//! exactly the same on every future tick, so retrying forever hides real debt
//! behind a repeating error line. Those reasons stall the consumer: the marker
//! is durable, evaluation suspends, a friction is filed, and only an audited
//! recovery or reset resumes it.
//!
//! Only a stuck reason is recorded. Marking every transient deferral would put
//! a fenced state write on the evaluator's hot path, where it would lose races
//! against the pass that is making progress.
//!
//! `history_diverged` is the one stuck reason with an automatic way out. A
//! rebase that preserved content is provable, so the evaluator runs the same
//! replay proof `recover --replay-history` previews and applies it under a
//! system-attributed audit record. Only an unprovable rewrite stalls.

use super::{DeliveryHost, Evaluation, recovery};
use crate::AutomationError;
use crate::checkpoint::diagnostic;
use chrono::{DateTime, Duration, Utc};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::recovery::{
    AutomationStall, DEFAULT_STALL_WINDOW_MINUTES, HistoryDivergence, RecoveryRequest, SYSTEM_ACTOR,
};
use orbit_types::workflow::automation::{AutomationDiagnostic, AutomationState};

/// The observed revision is no longer an ancestor of the configured head.
pub const HISTORY_DIVERGED: &str = "history_diverged";

/// Reasons that never clear themselves: every later evaluation reads the same
/// fact, so retrying is indistinguishable from doing nothing.
const STUCK: &[&str] = &[
    HISTORY_DIVERGED,
    "repository_changed",
    "provider_identity_missing",
    "state_missing",
];

/// How long any other deferred reason may persist before it is escalated.
/// Hosts override it from `automation.stall_window_minutes`.
pub const DEFAULT_WINDOW_MINUTES: u32 = DEFAULT_STALL_WINDOW_MINUTES;

/// How many obligations a divergence record names before it truncates.
const MAX_REPORTED_OBLIGATIONS: usize = 50;

/// Reason reported while a blocking stall suspends evaluation.
pub fn stalled_reason(reason: &str) -> String {
    format!("stalled: {reason}")
}

/// What the host needs to file one deduped friction about a stall.
pub struct StallReport<'a> {
    pub consumer: &'a str,
    pub repository: &'a str,
    pub branch: &'a str,
    pub reason: &'a str,
    /// Divergence facts, present only for `history_diverged`.
    pub divergence: Option<&'a HistoryDivergence>,
    /// True when the evaluator already repaired the consumer, so the friction
    /// records what happened rather than asking for operator action.
    pub repaired: bool,
    pub at: DateTime<Utc>,
}

fn is_stuck(reason: &str) -> bool {
    STUCK.contains(&reason)
}

/// The stalled diagnostic for a consumer evaluation may not resume, or `None`
/// when it is free to run.
///
/// Escalation to a warning and a friction happens here too, so a stall that
/// outlives the window is never silent. Clearing a stall updates `state` in
/// place, so the caller continues its pass against the generation the clearing
/// write left behind.
pub(super) fn suspended(
    store: &dyn AutomationStoreBackend,
    host: &dyn DeliveryHost,
    request: &Evaluation<'_>,
    state: &mut AutomationState,
) -> Result<Option<AutomationDiagnostic>, AutomationError> {
    let Some(stall) = state.stall.clone() else {
        return Ok(None);
    };

    // A rewrite that was undone, or a branch that grew back over the orphaned
    // revision, is provably no longer the problem the marker records. That is
    // the one condition a consumer may clear for itself: it needs no audit
    // because nothing about the retained debt changes.
    if stall.reason == HISTORY_DIVERGED
        && !request.dry_run
        && host.history_converged(&request.trigger.branch, state)?
    {
        *state = write(store, state, None)?;
        return Ok(None);
    }

    let state = if request.dry_run {
        state.clone()
    } else {
        escalate_if_due(store, host, request, state, stall.clone())?
    };

    diagnostic(
        store,
        request.consumer,
        &stalled_reason(&stall.reason),
        Some(state),
    )
    .map(Some)
}

/// Record what a deferred reason means for this consumer, and answer the tick.
///
/// A transient reason keeps today's behaviour: the error propagates and the
/// next tick retries. A stuck reason becomes a durable stall instead, and
/// `history_diverged` first gets one automatic chance to prove itself safe.
pub(super) fn deferred(
    store: &dyn AutomationStoreBackend,
    host: &dyn DeliveryHost,
    request: &Evaluation<'_>,
    reason: String,
) -> Result<AutomationDiagnostic, AutomationError> {
    let Some(state) = store.automation_state(request.consumer)? else {
        return Err(AutomationError::Deferred(reason));
    };

    if reason == HISTORY_DIVERGED {
        return diverged(store, host, request, state);
    }

    if !is_stuck(&reason) {
        return Err(AutomationError::Deferred(reason));
    }

    let state = mark(
        store,
        host,
        request,
        &state,
        AutomationStall {
            reason: reason.clone(),
            since: request.now,
            escalated_at: None,
            friction_id: None,
            divergence: None,
        },
    )?;

    diagnostic(
        store,
        request.consumer,
        &stalled_reason(&reason),
        Some(state),
    )
}

/// Prove the rebase, or stall on what the proof refused.
fn diverged(
    store: &dyn AutomationStoreBackend,
    host: &dyn DeliveryHost,
    request: &Evaluation<'_>,
    state: AutomationState,
) -> Result<AutomationDiagnostic, AutomationError> {
    let refusal = match replay(store, host, request, &state) {
        Ok(replayed) => return Ok(replayed),
        Err(AutomationError::Refused(refusal)) => refusal,
        // The proof could not run at all this pass. That is ordinary
        // backpressure, not evidence that the rewrite is unsafe.
        Err(error) => return Err(error),
    };

    let (_, head) = host.head(&request.trigger.branch)?;
    let stall = AutomationStall {
        reason: HISTORY_DIVERGED.into(),
        since: request.now,
        escalated_at: None,
        friction_id: None,
        divergence: Some(HistoryDivergence {
            observed: state.observed.clone(),
            head,
            refusal,
            obligations: obligations(&state),
        }),
    };
    let state = mark(store, host, request, &state, stall)?;

    diagnostic(
        store,
        request.consumer,
        &stalled_reason(HISTORY_DIVERGED),
        Some(state),
    )
}

/// Apply the replay proof under a system-attributed audit record, then record
/// the rewrite as friction so it does not pass unseen.
fn replay(
    store: &dyn AutomationStoreBackend,
    host: &dyn DeliveryHost,
    request: &Evaluation<'_>,
    state: &AutomationState,
) -> Result<AutomationDiagnostic, AutomationError> {
    let proof = host.replay_history(&request.trigger.branch, state)?;
    let divergence = HistoryDivergence {
        observed: proof.record.old_observed.clone(),
        head: proof.record.captured_head.clone(),
        refusal: String::new(),
        obligations: vec![],
    };

    let operation = recovery::Recovery {
        consumer: request.consumer,
        epoch: request.epoch,
        trigger: request.trigger,
        repository: &state.repository,
        host_refusal: None,
        request: &RecoveryRequest {
            replay_history: true,
            reason: "automatic replay of a proven content-preserving rebase".into(),
            ..RecoveryRequest::default()
        },
        by: SYSTEM_ACTOR,
        now: request.now,
        replay: Some(proof),
    };

    // The record is written first: only a committed replay may be reported as
    // one, and a refusal here has to reach the stall path instead.
    let applied = recovery::apply(store, &operation)?;
    host.report_stall(&StallReport {
        consumer: request.consumer,
        repository: &state.repository,
        branch: &request.trigger.branch,
        reason: HISTORY_DIVERGED,
        divergence: Some(&divergence),
        repaired: true,
        at: request.now,
    })?;
    tracing::info!(
        consumer = request.consumer,
        mappings = applied
            .history_replay
            .as_ref()
            .map(|record| record.mappings.len())
            .unwrap_or_default(),
        "automation replayed a diverged branch history"
    );

    diagnostic(
        store,
        request.consumer,
        "history_replayed",
        store.automation_state(request.consumer)?,
    )
}

/// Everything the consumer still owes, named so a reader of the friction can
/// see exactly what an unprovable rewrite left behind: each pending delivery
/// with the evidence that identified it, then each commit with no identity yet.
fn obligations(state: &AutomationState) -> Vec<String> {
    state
        .pending
        .iter()
        .map(|delivery| format!("{} ({})", delivery.key, delivery.evidence_reference))
        .chain(
            state
                .unresolved
                .iter()
                .map(|(commit, reason)| format!("commit {commit} ({reason})")),
        )
        .take(MAX_REPORTED_OBLIGATIONS)
        .collect()
}

/// Persist `stall`, carrying over the age and escalation of an existing marker
/// for the same reason so repeated ticks neither reset the clock nor file a
/// second friction.
fn mark(
    store: &dyn AutomationStoreBackend,
    host: &dyn DeliveryHost,
    request: &Evaluation<'_>,
    state: &AutomationState,
    mut stall: AutomationStall,
) -> Result<AutomationState, AutomationError> {
    if let Some(recorded) = state
        .stall
        .as_ref()
        .filter(|old| old.reason == stall.reason)
    {
        stall.since = recorded.since;
        stall.escalated_at = recorded.escalated_at;
        stall.friction_id = recorded.friction_id.clone();
        stall.divergence = stall.divergence.or_else(|| recorded.divergence.clone());
    }

    // A proven-unsafe history rewrite is reported the moment it is found: the
    // debt is already unreachable, so waiting out the window buys nothing.
    if stall.escalated_at.is_none()
        && (stall.reason == HISTORY_DIVERGED || due(host, request.now, &stall))
    {
        stall.friction_id = host.report_stall(&StallReport {
            consumer: request.consumer,
            repository: &state.repository,
            branch: &request.trigger.branch,
            reason: &stall.reason,
            divergence: stall.divergence.as_ref(),
            repaired: false,
            at: request.now,
        })?;
        stall.escalated_at = Some(request.now);
        tracing::warn!(
            consumer = request.consumer,
            reason = stall.reason,
            since = %stall.since,
            friction_id = stall.friction_id.as_deref().unwrap_or_default(),
            "delivery automation is stalled and needs an operator"
        );
    }

    write(store, state, Some(stall))
}

/// Escalate a stall already recorded once its window has elapsed.
fn escalate_if_due(
    store: &dyn AutomationStoreBackend,
    host: &dyn DeliveryHost,
    request: &Evaluation<'_>,
    state: &AutomationState,
    stall: AutomationStall,
) -> Result<AutomationState, AutomationError> {
    if stall.escalated_at.is_some() || !due(host, request.now, &stall) {
        return Ok(state.clone());
    }

    mark(store, host, request, state, stall)
}

/// True once the reason has persisted for the host's escalation window.
fn due(host: &dyn DeliveryHost, now: DateTime<Utc>, stall: &AutomationStall) -> bool {
    let window = Duration::minutes(i64::from(host.stall_window_minutes().max(1)));
    now.signed_duration_since(stall.since) >= window
}

fn write(
    store: &dyn AutomationStoreBackend,
    state: &AutomationState,
    stall: Option<AutomationStall>,
) -> Result<AutomationState, AutomationError> {
    if state.stall == stall {
        return Ok(state.clone());
    }

    let mut next = state.clone();
    next.stall = stall;
    next.generation = state
        .generation
        .checked_add(1)
        .ok_or_else(|| AutomationError::Deferred("generation_exhausted".into()))?;

    if !store.automation_stall(state, &next)? {
        return Err(AutomationError::Deferred("concurrent_evaluation".into()));
    }

    Ok(next)
}
