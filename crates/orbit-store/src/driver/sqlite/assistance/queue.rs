use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::assistance::{AssistanceReason, assistance_digest};
use rusqlite::{Connection, OptionalExtension, params};

use super::{assistance_complete_gap, assistance_decode, assistance_json};
use crate::contracts::assistance::{AssistanceAdmission, AssistanceClaim, AssistanceWork};

pub(super) fn assistance_insert_work(
    conn: &Connection,
    work: &AssistanceWork,
) -> Result<AssistanceAdmission, OrbitError> {
    let existing: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM assistance_work WHERE id=?1)",
            [&work.id],
            |r| r.get(0),
        )
        .map_err(sqlite_store_error)?;
    if existing {
        return Ok(AssistanceAdmission::Existing(work.id.clone()));
    }
    conn.execute(
        "INSERT OR IGNORE INTO assistance_budgets(epoch) VALUES (?1)",
        [&work.budget.epoch],
    )
    .map_err(sqlite_store_error)?;
    let (pending, total): (u32, u32) = conn
        .query_row(
            "SELECT COALESCE(SUM(state!='done'),0), COUNT(*) FROM assistance_work",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_store_error)?;
    if pending >= work.limits.queue_capacity || total >= work.limits.max_records {
        conn.execute(
            "UPDATE assistance_budgets SET queue_rejections=queue_rejections+1 WHERE epoch=?1",
            [&work.budget.epoch],
        )
        .map_err(sqlite_store_error)?;
        // A rejected newer snapshot must invalidate the old suggestion too.
        conn.execute("DELETE FROM assistance_slots WHERE slot=?1", [&work.slot])
            .map_err(sqlite_store_error)?;
        return Ok(AssistanceAdmission::QueueFull);
    }
    let expires = work.created_at + Duration::seconds(work.limits.queue_age_seconds.into());
    conn.execute(
        "INSERT INTO assistance_work(id,slot,workspace_id,use_case,policy_digest,payload_json,created_at,expires_at,retain_until,state,ready_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'queued',?7)",
        params![work.id,work.slot,work.request.workspace_id,work.request.use_case,work.policy_digest,assistance_json(work)?,
            work.created_at.timestamp_millis(),expires.timestamp_millis(),(expires+Duration::seconds(work.limits.retention_seconds.into())).timestamp_millis()],
    ).map_err(sqlite_store_error)?;
    conn.execute("INSERT INTO assistance_slots(slot,request_id) VALUES (?1,?2) ON CONFLICT(slot) DO UPDATE SET request_id=excluded.request_id",
        params![work.slot,work.id]).map_err(sqlite_store_error)?;
    // Coalesce queued snapshots without disturbing an in-flight claim.
    conn.execute("UPDATE assistance_work SET state='done', result_json=?3 WHERE slot=?1 AND id!=?2 AND state='queued'",
        params![work.slot,work.id,assistance_json(&orbit_types::assistance::AssistanceResult::assistance_gap(AssistanceReason::Superseded))?]).map_err(sqlite_store_error)?;
    Ok(AssistanceAdmission::Enqueued(work.id.clone()))
}

pub(super) fn assistance_reserve_claim(
    conn: &Connection,
    workspace: &str,
    use_case: &str,
    policy_digest: &str,
    now: DateTime<Utc>,
) -> Result<Option<AssistanceClaim>, OrbitError> {
    let row: Option<(String,u32)> = conn.query_row(
        "SELECT payload_json, attempt_count FROM assistance_work w WHERE workspace_id=?1 AND use_case=?2 AND policy_digest=?3
         AND state='queued' AND ready_at<=?4 AND EXISTS(SELECT 1 FROM assistance_slots s WHERE s.request_id=w.id)
         ORDER BY created_at,id LIMIT 1", params![workspace,use_case,policy_digest,now.timestamp_millis()],
        |r| Ok((r.get(0)?,r.get(1)?)),
    ).optional().map_err(sqlite_store_error)?;
    let Some((payload, count)) = row else {
        return Ok(None);
    };
    let work: AssistanceWork = assistance_decode(&payload)?;
    let active: u32 = conn
        .query_row(
            "SELECT COUNT(*) FROM assistance_work WHERE state='claimed'",
            [],
            |r| r.get(0),
        )
        .map_err(sqlite_store_error)?;
    if active >= work.limits.concurrency {
        return Ok(None);
    }
    let Some(charge) = work
        .budget
        .assistance_charge_bound(work.limits.max_input_tokens.into())
    else {
        assistance_complete_gap(conn, &work.id, AssistanceReason::PricingUnknown)?;
        return Ok(None);
    };
    let (tokens, spend): (u64, u64) = conn
        .query_row(
            "SELECT reserved_tokens,reserved_charge FROM assistance_budgets WHERE epoch=?1",
            [&work.budget.epoch],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_store_error)?;
    let next_tokens = tokens.checked_add(work.limits.max_input_tokens.into());
    let next_spend = spend.checked_add(charge);
    if count >= work.limits.max_attempts
        || next_tokens.is_none_or(|n| n > work.budget.token_ceiling || n > i64::MAX as u64)
        || next_spend.is_none_or(|n| n > work.budget.spend_ceiling_microusd || n > i64::MAX as u64)
    {
        assistance_complete_gap(conn, &work.id, AssistanceReason::BudgetExhausted)?;
        return Ok(None);
    }
    let ordinal = count + 1;
    let attempt_id = assistance_digest(format!("{}:{ordinal}", work.id).as_bytes());
    // Share one exact millisecond deadline with SQLite and the transport.
    // Rounding down a sub-millisecond admission would expire its claim early.
    let deadline = DateTime::from_timestamp_millis(
        (now + Duration::seconds(work.limits.timeout_seconds.into())).timestamp_millis() + 1,
    )
    .ok_or_else(|| OrbitError::Store("assistance deadline is out of range".into()))?;
    conn.execute("UPDATE assistance_budgets SET reserved_tokens=reserved_tokens+?2, reserved_charge=reserved_charge+?3, attempts=attempts+1 WHERE epoch=?1",
        params![work.budget.epoch,work.limits.max_input_tokens,charge]).map_err(sqlite_store_error)?;
    conn.execute("INSERT INTO assistance_attempts(attempt_id,request_id,ordinal,started_at,deadline,reserved_tokens,reserved_charge)
        VALUES (?1,?2,?3,?4,?5,?6,?7)", params![attempt_id,work.id,ordinal,now.timestamp_millis(),deadline.timestamp_millis(),work.limits.max_input_tokens,charge]).map_err(sqlite_store_error)?;
    conn.execute("UPDATE assistance_work SET state='claimed',attempt_count=?2,claim_id=?3,claim_until=?4 WHERE id=?1",
        params![work.id,ordinal,attempt_id,deadline.timestamp_millis()]).map_err(sqlite_store_error)?;
    Ok(Some(AssistanceClaim {
        work,
        attempt_id,
        ordinal,
        deadline,
    }))
}
