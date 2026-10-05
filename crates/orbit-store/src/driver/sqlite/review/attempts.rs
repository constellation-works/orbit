//! Review attempt reservation and settlement against the ledger budget.

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::workflow::{
    ReviewAttempt, ReviewAttemptState, ReviewLedger, ReviewReservation, ReviewVerdict,
    ReviewerInvocation, ReviewerInvocationEvent,
};
use rusqlite::Connection;

use super::ledger::write_ledger;
use crate::contracts::ReviewReserveRequest;

fn attempt_id(lineage_key: &str, index: u32) -> String {
    let digest = sha256_hex(lineage_key.as_bytes());
    format!("rvw-{}-{index}", &digest[..12])
}

/// Mark an attempt settled, charging its reviewer runtime at `now`. A
/// released attempt's provisional charge is replaced. Returns false when
/// the attempt is unknown.
pub(super) fn settle_attempt(
    ledger: &mut ReviewLedger,
    attempt_id: &str,
    verdict: ReviewVerdict,
    now: DateTime<Utc>,
) -> bool {
    let Some(attempt) = ledger
        .attempts
        .iter_mut()
        .find(|attempt| attempt.attempt_id == attempt_id)
    else {
        return false;
    };
    let previous = match attempt.released_at.take() {
        Some(_) => attempt.elapsed_seconds.unwrap_or(0),
        None => 0,
    };
    let charge = stop_reviewer(attempt, now);
    attempt.state = ReviewAttemptState::Settled { verdict };
    attempt.elapsed_seconds = Some(charge);
    ledger.consumed_seconds = ledger
        .consumed_seconds
        .saturating_sub(previous)
        .saturating_add(charge);
    true
}

/// Close an attempt that has no reviewer verdict: settle it `incomplete`,
/// charging its reviewer runtime with a still-running invocation counted up
/// to `bound`, and mark it released so a resumed run may still settle it
/// with a verdict. Releasing an already released attempt replaces its
/// charge. Returns false when the attempt is unknown or was settled with a
/// verdict, which a release never rewrites.
pub(super) fn release_attempt(
    ledger: &mut ReviewLedger,
    attempt_id: &str,
    bound: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    let Some(attempt) = ledger
        .attempts
        .iter_mut()
        .find(|attempt| attempt.attempt_id == attempt_id)
    else {
        return false;
    };
    let previous = match (&attempt.state, attempt.released_at) {
        (ReviewAttemptState::Open, _) => 0,
        (ReviewAttemptState::Settled { .. }, Some(_)) => attempt.elapsed_seconds.unwrap_or(0),
        (ReviewAttemptState::Settled { .. }, None) => return false,
    };
    let charge = stop_reviewer(attempt, bound);
    attempt.state = ReviewAttemptState::Settled {
        verdict: ReviewVerdict::Incomplete,
    };
    attempt.elapsed_seconds = Some(charge);
    attempt.released_at = Some(now);
    ledger.consumed_seconds = ledger
        .consumed_seconds
        .saturating_sub(previous)
        .saturating_add(charge);
    true
}

/// Fold a still-running reviewer invocation into the attempt's finished
/// runtime, counted up to `bound`, and return the attempt's total.
fn stop_reviewer(attempt: &mut ReviewAttempt, bound: DateTime<Utc>) -> u64 {
    attempt.reviewer_seconds = attempt.reviewer_runtime_at(bound);
    attempt.reviewer_running = None;
    attempt.reviewer_seconds
}

/// Apply a reviewer invocation event. Returns false when the attempt is
/// unknown or settled with a verdict, which no later invocation changes.
pub(super) fn record_invocation(
    ledger: &mut ReviewLedger,
    attempt_id: &str,
    run_id: &str,
    event: ReviewerInvocationEvent,
    now: DateTime<Utc>,
) -> bool {
    let Some(attempt) = ledger
        .attempts
        .iter_mut()
        .find(|attempt| attempt.attempt_id == attempt_id)
    else {
        return false;
    };
    if attempt.released_at.is_none() && matches!(attempt.state, ReviewAttemptState::Settled { .. })
    {
        return false;
    }
    match event {
        ReviewerInvocationEvent::Started { timeout_seconds } => {
            // A start the previous invocation never reported finishing
            // means that invocation is gone; charge it up to this start.
            stop_reviewer(attempt, now);
            let timeout = Duration::seconds(i64::try_from(timeout_seconds).unwrap_or(i64::MAX));
            attempt.reviewer_running = Some(ReviewerInvocation {
                run_id: run_id.to_string(),
                started_at: now,
                deadline: now.checked_add_signed(timeout).unwrap_or(now),
            });
        }
        ReviewerInvocationEvent::Finished { runtime_seconds } => {
            attempt.reviewer_running = None;
            attempt.reviewer_seconds = attempt.reviewer_seconds.saturating_add(runtime_seconds);
        }
    }
    true
}

pub(super) fn reserve_new(
    conn: &Connection,
    workspace_id: &str,
    ledger: ReviewLedger,
    request: &ReviewReserveRequest<'_>,
) -> Result<(ReviewReservation, ReviewLedger), OrbitError> {
    let revision = ledger.revision;
    reserve_new_in(conn, workspace_id, Some(revision), ledger, request)
}

pub(super) fn reserve_new_in(
    conn: &Connection,
    workspace_id: &str,
    previous_revision: Option<u32>,
    mut ledger: ReviewLedger,
    request: &ReviewReserveRequest<'_>,
) -> Result<(ReviewReservation, ReviewLedger), OrbitError> {
    let consumed = ledger.consumed();
    if consumed.reviewer_starts >= ledger.budget.reviewer_starts {
        return Ok((
            ReviewReservation::Exhausted {
                reason: "review_starts_exhausted",
                consumed,
            },
            ledger,
        ));
    }
    if consumed.seconds >= u64::from(ledger.budget.minutes).saturating_mul(60) {
        return Ok((
            ReviewReservation::Exhausted {
                reason: "review_minutes_exhausted",
                consumed,
            },
            ledger,
        ));
    }

    let index = ledger
        .attempts
        .last()
        .map_or(0, |a| a.index)
        .checked_add(1)
        .ok_or_else(|| OrbitError::Store("review attempt index exhausted".into()))?;
    let attempt = ReviewAttempt {
        attempt_id: attempt_id(request.lineage_key, index),
        index,
        run_id: request.run_id.to_string(),
        task_meaning_digest: request.task_meaning_digest.to_string(),
        candidate: request.candidate.clone(),
        started_at: request.now,
        state: ReviewAttemptState::Open,
        elapsed_seconds: None,
        released_at: None,
        reviewer_seconds: 0,
        reviewer_running: None,
    };
    ledger.attempts.push(attempt.clone());
    write_ledger(
        conn,
        workspace_id,
        previous_revision,
        &mut ledger,
        request.now,
    )?;
    Ok((ReviewReservation::Reserved { attempt }, ledger))
}
