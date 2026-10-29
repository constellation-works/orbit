//! Independent, short-transaction assistance queue and conservative charges.

mod queue;
mod settlement;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::assistance::{
    AssistanceDisposition, AssistanceMode, AssistanceReason, AssistanceResult,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};

use crate::Store;
use crate::contracts::assistance::{
    AssistanceAccounting, AssistanceAdmission, AssistanceAttempt, AssistanceClaim,
    AssistanceRecord, AssistanceStoreBackend, AssistanceWork,
};
use settlement::AssistanceOutbound;

pub(super) fn assistance_json<T: Serialize>(value: &T) -> Result<String, OrbitError> {
    serde_json::to_string(value)
        .map_err(|_| OrbitError::Store("cannot encode assistance record".into()))
}

pub(super) fn assistance_decode<T: DeserializeOwned>(value: &str) -> Result<T, OrbitError> {
    serde_json::from_str(value)
        .map_err(|_| OrbitError::Store("unsupported or malformed assistance record".into()))
}

impl AssistanceStoreBackend for Store {
    fn assistance_enqueue(&self, work: &AssistanceWork) -> Result<AssistanceAdmission, OrbitError> {
        if work.mode == AssistanceMode::Off {
            return Ok(AssistanceAdmission::Disabled);
        }
        work.request
            .validate_request()
            .map_err(|_| OrbitError::InvalidInput("invalid assistance request".into()))?;
        work.limits
            .validate_limits()
            .map_err(|_| OrbitError::InvalidInput("invalid assistance limits".into()))?;
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            settlement::assistance_maintain_rows(tx.connection(), work.created_at)?;
            queue::assistance_insert_work(tx.connection(), work)
        })
    }

    fn assistance_claim(
        &self,
        workspace: &str,
        use_case: &str,
        policy_digest: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<AssistanceClaim>, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            settlement::assistance_maintain_rows(conn, now)?;
            queue::assistance_reserve_claim(conn, workspace, use_case, policy_digest, now)
        })
    }

    fn assistance_finish(
        &self,
        claim: &AssistanceClaim,
        mut result: AssistanceResult,
        now: DateTime<Utc>,
    ) -> Result<bool, OrbitError> {
        result.validate_result(&claim.work.request.contract);
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            settlement::assistance_settle_claim(
                tx.connection(),
                claim,
                result,
                now,
                AssistanceOutbound::Attempted,
            )
        })
    }

    fn assistance_finish_unattempted(
        &self,
        claim: &AssistanceClaim,
        mut result: AssistanceResult,
        now: DateTime<Utc>,
    ) -> Result<bool, OrbitError> {
        result.validate_result(&claim.work.request.contract);
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            settlement::assistance_settle_claim(
                tx.connection(),
                claim,
                result,
                now,
                AssistanceOutbound::Unattempted,
            )
        })
    }

    fn assistance_record(&self, id: &str) -> Result<Option<AssistanceRecord>, OrbitError> {
        let conn = self.read()?;
        let row: Option<(String, Option<String>, bool, bool)> = conn.query_row(
            "SELECT w.payload_json, w.result_json, EXISTS(SELECT 1 FROM assistance_slots s WHERE s.request_id=w.id), w.state='done'
             FROM assistance_work w WHERE w.id=?1", [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        ).optional().map_err(sqlite_store_error)?;
        let Some((payload, result, current, settled)) = row else {
            return Ok(None);
        };
        let mut stmt = conn.prepare(
            "SELECT attempt_id, ordinal, outcome_unknown, reserved_tokens, reserved_charge, result_json, late_result_json
             FROM assistance_attempts WHERE request_id=?1 ORDER BY ordinal",
        ).map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map([id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, u32>(1)?,
                    r.get::<_, bool>(2)?,
                    r.get::<_, u64>(3)?,
                    r.get::<_, u64>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(sqlite_store_error)?;
        let mut attempts = Vec::new();
        for row in rows {
            let (
                attempt_id,
                ordinal,
                outcome_unknown,
                reserved_tokens,
                reserved_charge_microusd,
                result,
                late_result,
            ) = row.map_err(sqlite_store_error)?;
            attempts.push(AssistanceAttempt {
                attempt_id,
                ordinal,
                outcome_unknown,
                reserved_tokens,
                reserved_charge_microusd,
                result: result.as_deref().map(assistance_decode).transpose()?,
                late_result: late_result.as_deref().map(assistance_decode).transpose()?,
            });
        }
        let mut result: Option<AssistanceResult> =
            result.as_deref().map(assistance_decode).transpose()?;
        if !current && let Some(result) = &mut result {
            result.disposition = AssistanceDisposition::Stale;
            result.reason = AssistanceReason::Superseded;
        }
        Ok(Some(AssistanceRecord {
            work: assistance_decode(&payload)?,
            current,
            settled,
            result,
            attempts,
        }))
    }

    fn assistance_accounting(&self, epoch: &str) -> Result<AssistanceAccounting, OrbitError> {
        let conn = self.read()?;
        Ok(conn.query_row(
            "SELECT reserved_tokens, reserved_charge, attempts, unknown_attempts, queue_rejections FROM assistance_budgets WHERE epoch=?1",
            [epoch], |r| Ok(AssistanceAccounting { reserved_tokens: r.get(0)?, reserved_charge_microusd: r.get(1)?,
                attempts: r.get(2)?, outcome_unknown_attempts: r.get(3)?, queue_rejections: r.get(4)? }),
        ).optional().map_err(sqlite_store_error)?.unwrap_or_default())
    }

    fn assistance_maintain(&self, now: DateTime<Utc>) -> Result<(), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            settlement::assistance_maintain_rows(tx.connection(), now)
        })
    }
}

pub(super) fn assistance_complete_gap(
    conn: &Connection,
    id: &str,
    reason: AssistanceReason,
) -> Result<(), OrbitError> {
    conn.execute("UPDATE assistance_work SET state='done', claim_id=NULL, claim_until=NULL, result_json=?2 WHERE id=?1",
        params![id, assistance_json(&AssistanceResult::assistance_gap(reason))?]).map_err(sqlite_store_error)?;
    Ok(())
}
