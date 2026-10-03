//! Review attempt reservation and settlement against the ledger budget.

use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::workflow::{
    ReviewAttempt, ReviewAttemptState, ReviewLedger, ReviewReservation, ReviewVerdict,
};
use rusqlite::Connection;

use super::ledger::write_ledger;
use crate::contracts::ReviewReserveRequest;

fn attempt_id(lineage_key: &str, index: u32) -> String {
    let digest = sha256_hex(lineage_key.as_bytes());
    format!("rvw-{}-{index}", &digest[..12])
}

/// Mark an attempt settled. Returns false when the attempt is unknown.
pub(super) fn settle_attempt(
    ledger: &mut ReviewLedger,
    attempt_id: &str,
    verdict: ReviewVerdict,
    repair_cycles: u32,
    elapsed_seconds: u64,
) -> bool {
    let Some(attempt) = ledger
        .attempts
        .iter_mut()
        .find(|attempt| attempt.attempt_id == attempt_id)
    else {
        return false;
    };
    attempt.state = ReviewAttemptState::Settled { verdict };
    attempt.repair_cycles = repair_cycles;
    attempt.elapsed_seconds = Some(elapsed_seconds);
    ledger.consumed_seconds = ledger.consumed_seconds.saturating_add(elapsed_seconds);
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

    let index = consumed.reviewer_starts.saturating_add(1);
    let attempt = ReviewAttempt {
        attempt_id: attempt_id(request.lineage_key, index),
        index,
        run_id: request.run_id.to_string(),
        task_meaning_digest: request.task_meaning_digest.to_string(),
        candidate: request.candidate.clone(),
        started_at: request.now,
        state: ReviewAttemptState::Open,
        repair_cycles: 0,
        elapsed_seconds: None,
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
