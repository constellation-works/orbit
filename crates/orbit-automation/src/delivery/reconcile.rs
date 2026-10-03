//! Settle admitted work against Core's outcome and retire proven-covered prefixes.

use super::{ActionOutcome, DeliveryHost, evidence, observe};
use crate::AutomationError;
use crate::checkpoint::commit;
use chrono::{DateTime, Utc};
use orbit_store::contracts::AutomationStoreBackend;
use orbit_types::workflow::automation::*;

pub(super) fn retire_covered_prefix(
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

pub(super) fn reconcile(
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
