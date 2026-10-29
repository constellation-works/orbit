use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::assistance::{AssistanceDisposition, AssistanceReason, AssistanceResult};
use rusqlite::{Connection, OptionalExtension, params};

use super::{assistance_complete_gap, assistance_decode, assistance_json};
use crate::contracts::assistance::{AssistanceClaim, AssistanceWork};

struct AssistanceSettlementRow {
    state: String,
    claim_id: Option<String>,
    deadline: Option<i64>,
    expires: i64,
    current: bool,
    payload: String,
    ordinal: u32,
}

/// Whether the claim's request entered outbound I/O. Only an unattempted claim
/// releases its reservation and is exempt from outcome-unknown accounting.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum AssistanceOutbound {
    Attempted,
    Unattempted,
}

pub(super) fn assistance_maintain_rows(
    conn: &Connection,
    now: DateTime<Utc>,
) -> Result<(), OrbitError> {
    let mut stmt = conn.prepare("SELECT payload_json,claim_id,attempt_count FROM assistance_work WHERE state='claimed' AND claim_until<=?1").map_err(sqlite_store_error)?;
    let expired = stmt
        .query_map([now.timestamp_millis()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u32>(2)?,
            ))
        })
        .map_err(sqlite_store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)?;
    drop(stmt);
    for (payload, attempt_id, ordinal) in expired {
        let work: AssistanceWork = assistance_decode(&payload)?;
        let claim = AssistanceClaim {
            work,
            attempt_id,
            ordinal,
            deadline: now,
        };
        assistance_settle_claim(
            conn,
            &claim,
            AssistanceResult::assistance_gap(AssistanceReason::ClaimExpiredUnknown),
            now,
            AssistanceOutbound::Attempted,
        )?;
    }
    conn.execute("UPDATE assistance_work SET state='done',result_json=?2 WHERE state='queued' AND expires_at<=?1",
        params![now.timestamp_millis(),assistance_json(&AssistanceResult::assistance_gap(AssistanceReason::QueueExpired))?]).map_err(sqlite_store_error)?;
    conn.execute(
        "DELETE FROM assistance_work WHERE state='done' AND retain_until<=?1",
        [now.timestamp_millis()],
    )
    .map_err(sqlite_store_error)?;
    Ok(())
}

pub(super) fn assistance_settle_claim(
    conn: &Connection,
    claim: &AssistanceClaim,
    mut result: AssistanceResult,
    now: DateTime<Utc>,
    outbound: AssistanceOutbound,
) -> Result<bool, OrbitError> {
    let state: Option<AssistanceSettlementRow> = conn.query_row(
        "SELECT state,claim_id,claim_until,expires_at,EXISTS(SELECT 1 FROM assistance_slots s WHERE s.request_id=w.id),payload_json,attempt_count
         FROM assistance_work w WHERE id=?1", [&claim.work.id], |r| Ok(AssistanceSettlementRow {
            state:r.get(0)?,claim_id:r.get(1)?,deadline:r.get(2)?,expires:r.get(3)?,current:r.get(4)?,payload:r.get(5)?,ordinal:r.get(6)? }),
    ).optional().map_err(sqlite_store_error)?;
    let Some(AssistanceSettlementRow {
        state,
        claim_id,
        deadline,
        expires,
        current,
        payload,
        ordinal,
    }) = state
    else {
        return Ok(false);
    };
    let work: AssistanceWork = assistance_decode(&payload)?;
    result.validate_result(&work.request.contract);
    if result
        .returned_model
        .as_ref()
        .is_some_and(|m| m != &work.requested_model && m != &work.model_revision)
    {
        result = AssistanceResult::assistance_gap(AssistanceReason::Malformed);
    }
    if result.disposition == AssistanceDisposition::Suggested
        && (work.request.evidence_packet.truncated
            || !work.request.evidence_packet.omissions.is_empty())
    {
        result.disposition = AssistanceDisposition::Uncertain;
    }
    if state != "claimed" || claim_id.as_deref() != Some(&claim.attempt_id) {
        result.disposition = AssistanceDisposition::Stale;
        result.reason = AssistanceReason::Superseded;
        result.answers.clear();
        conn.execute("UPDATE assistance_attempts SET late_result_json=COALESCE(late_result_json,?2) WHERE attempt_id=?1 AND request_id=?3",
            params![claim.attempt_id,assistance_json(&result)?,claim.work.id]).map_err(sqlite_store_error)?;
        return Ok(false);
    }
    let unknown = outbound == AssistanceOutbound::Attempted
        && (deadline.is_none_or(|d| now.timestamp_millis() >= d)
            || matches!(
                result.reason,
                AssistanceReason::TimeoutUnknown | AssistanceReason::ClaimExpiredUnknown
            ));
    if unknown {
        result = AssistanceResult::assistance_gap(
            if result.reason == AssistanceReason::ClaimExpiredUnknown {
                AssistanceReason::ClaimExpiredUnknown
            } else {
                AssistanceReason::TimeoutUnknown
            },
        );
        conn.execute(
            "UPDATE assistance_budgets SET unknown_attempts=unknown_attempts+1 WHERE epoch=?1",
            [&work.budget.epoch],
        )
        .map_err(sqlite_store_error)?;
    }
    if outbound == AssistanceOutbound::Unattempted {
        assistance_release_reservation(conn, claim, &work)?;
    }
    conn.execute(
        "UPDATE assistance_attempts SET outcome_unknown=?2,result_json=?3 WHERE attempt_id=?1",
        params![claim.attempt_id, unknown, assistance_json(&result)?],
    )
    .map_err(sqlite_store_error)?;
    if !current {
        assistance_complete_gap(conn, &claim.work.id, AssistanceReason::Superseded)?;
        return Ok(false);
    }
    if now.timestamp_millis() >= expires {
        assistance_complete_gap(conn, &claim.work.id, AssistanceReason::QueueExpired)?;
        return Ok(false);
    }
    // Bounded retry is only for transient outcomes: unknown dispatches and fully
    // received timeout/rate-limit/server statuses (Unavailable). Definitive
    // client rejections (Refused) and malformed responses complete here.
    let retry = (unknown || result.reason == AssistanceReason::Unavailable)
        && ordinal < work.limits.max_attempts;
    conn.execute("UPDATE assistance_work SET state=?2,ready_at=?3,claim_id=NULL,claim_until=NULL,result_json=?4 WHERE id=?1",
        params![claim.work.id,if retry { "queued" } else { "done" },
            now.timestamp_millis()+i64::from(work.limits.retry_delay_seconds)*1000,assistance_json(&result)?]).map_err(sqlite_store_error)?;
    Ok(!retry && !unknown)
}

/// Returns an unattempted claim's reservation to its epoch, restoring the epoch
/// counters to their pre-claim values. The attempt row stays as an audit record
/// with zero reserved values, so per-attempt reservations still sum to the epoch.
fn assistance_release_reservation(
    conn: &Connection,
    claim: &AssistanceClaim,
    work: &AssistanceWork,
) -> Result<(), OrbitError> {
    let (tokens, charge): (u64, u64) = conn
        .query_row(
            "SELECT reserved_tokens,reserved_charge FROM assistance_attempts WHERE attempt_id=?1",
            [&claim.attempt_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_store_error)?;
    conn.execute(
        "UPDATE assistance_budgets SET reserved_tokens=reserved_tokens-?2, reserved_charge=reserved_charge-?3, attempts=attempts-1 WHERE epoch=?1",
        params![&work.budget.epoch, tokens, charge],
    )
    .map_err(sqlite_store_error)?;
    conn.execute(
        "UPDATE assistance_attempts SET reserved_tokens=0, reserved_charge=0 WHERE attempt_id=?1",
        [&claim.attempt_id],
    )
    .map_err(sqlite_store_error)?;
    Ok(())
}
