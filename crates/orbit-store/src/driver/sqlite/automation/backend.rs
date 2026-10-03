//! AutomationStoreBackend state, receipt and checkpoint queries.

use crate::Store;
use crate::contracts::AutomationStoreBackend;
use orbit_common::OrbitError;
use orbit_types::workflow::automation::recovery::RecoveryRecord;
use orbit_types::workflow::automation::{AcceptedCoverage, AutomationState};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::codec::{decode, encode};
use super::transition::validate_transition;
use super::{intents, recovery, waivers};

impl AutomationStoreBackend for Store {
    fn automation_waive(
        &self,
        previous: &AutomationState,
        next: &AutomationState,
        waiver: &orbit_types::workflow::automation::BatchWaiver,
    ) -> Result<bool, OrbitError> {
        waivers::commit(self, previous, next, waiver)
    }

    fn automation_waivers(
        &self,
        consumer: &str,
        limit: usize,
    ) -> Result<Vec<orbit_types::workflow::automation::BatchWaiver>, OrbitError> {
        waivers::list(self, consumer, limit)
    }

    fn automation_recover(
        &self,
        previous: &AutomationState,
        next: &AutomationState,
        record: &RecoveryRecord,
    ) -> Result<bool, OrbitError> {
        recovery::commit(self, previous, next, record)
    }

    fn automation_reset(
        &self,
        previous: &AutomationState,
        record: &RecoveryRecord,
    ) -> Result<bool, OrbitError> {
        recovery::reset(self, previous, record)
    }

    fn automation_stall(
        &self,
        previous: &AutomationState,
        next: &AutomationState,
    ) -> Result<bool, OrbitError> {
        recovery::stall(self, previous, next)
    }

    fn automation_recoveries(
        &self,
        consumer: &str,
        limit: usize,
    ) -> Result<Vec<RecoveryRecord>, OrbitError> {
        recovery::list(self, consumer, limit)
    }

    fn automation_receipt(
        &self,
        consumer: &str,
        batch: &str,
    ) -> Result<Option<AcceptedCoverage>, OrbitError> {
        self.with_read_connection(|conn| {
            let raw: Option<String> = conn
                .query_row(
                    "SELECT receipt_json FROM automation_coverage WHERE consumer=?1 AND batch_id=?2",
                    params![consumer, batch],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            raw.as_deref().map(decode).transpose()
        })
    }

    fn automation_record_delivery_intent(
        &self,
        delivery: &orbit_types::workflow::automation::Delivery,
    ) -> Result<(), OrbitError> {
        intents::record(self, delivery)
    }

    fn automation_delivery_intents(
        &self,
        repository: &str,
        branch: &str,
        commits: &[String],
    ) -> Result<Vec<orbit_types::workflow::automation::Delivery>, OrbitError> {
        intents::lookup(self, repository, branch, commits)
    }

    fn automation_state(&self, consumer: &str) -> Result<Option<AutomationState>, OrbitError> {
        self.with_read_connection(|conn| {
            let raw: Option<String> = conn
                .query_row(
                    "SELECT state_json FROM automation_consumers WHERE consumer=?1",
                    [consumer],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            raw.as_deref().map(decode).transpose()
        })
    }

    fn automation_initialize(&self, state: &AutomationState) -> Result<bool, OrbitError> {
        if state
            .members
            .as_ref()
            .is_some_and(|members| members != &Default::default())
            || state.generation != 0
            || state.baseline != state.observed
            || state.baseline != state.covered
            || state.active.is_some()
            || state.stall.is_some()
            || !state.pending.is_empty()
            || !state.waived.is_empty()
            || !state.pending_commits.is_empty()
            || !state.unresolved.is_empty()
        {
            return Err(OrbitError::InvalidInput(
                "invalid automation baseline".into(),
            ));
        }
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            tx.connection()
                .execute(
                    "INSERT OR IGNORE INTO automation_consumers VALUES (?1,0,?2)",
                    params![state.consumer, encode(state)?],
                )
                .map(|n| n == 1)
                .map_err(|e| OrbitError::Store(e.to_string()))
        })
    }

    fn automation_commit(
        &self,
        previous: &AutomationState,
        next: &AutomationState,
        receipt: Option<&AcceptedCoverage>,
    ) -> Result<bool, OrbitError> {
        validate_transition(previous, next, receipt)?;
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();

            // The generation and the exact prior state both fence the write, so a
            // concurrent evaluation that already moved the consumer changes nothing.
            let changed = conn
                .execute(
                    "UPDATE automation_consumers SET generation=?1,state_json=?2 WHERE consumer=?3 AND generation=?4 AND state_json=?5",
                    params![
                        next.generation,
                        encode(next)?,
                        previous.consumer,
                        previous.generation,
                        encode(previous)?
                    ],
                )
                .map_err(|e| OrbitError::Store(e.to_string()))?;

            if changed == 0 {
                return Ok(false);
            }

            if let Some(receipt) = receipt {
                // Preserve shipped delivery receipt bytes; member records carry
                // only their frozen action, never the unrelated consumer inventory.
                let batch_json = if let Some(members) = &previous.members {
                    encode(&serde_json::json!({"state_member": members.active}))?
                } else {
                    encode(&previous.active)?
                };

                conn.execute(
                    "INSERT INTO automation_coverage VALUES (?1,?2,?3,?4,?5)",
                    params![
                        receipt.batch_id,
                        previous.consumer,
                        batch_json,
                        encode(receipt)?,
                        receipt.accepted_at.to_rfc3339()
                    ],
                )
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            }

            Ok(true)
        })
    }

    fn automation_states(
        &self,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<AutomationState>, OrbitError> {
        self.with_read_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT state_json FROM automation_consumers WHERE consumer LIKE ?1 ESCAPE '\\' ORDER BY consumer LIMIT ?2",
                )
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            let pattern = format!("{}%", prefix.replace('%', "\\%").replace('_', "\\_"));
            let rows = stmt
                .query_map(params![pattern, limit.min(100)], |row| row.get::<_, String>(0))
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            rows.map(|row| {
                row.map_err(|e| OrbitError::Store(e.to_string()))
                    .and_then(|raw| decode(&raw))
            })
            .collect()
        })
    }

    fn automation_receipts(
        &self,
        consumer: &str,
        limit: usize,
    ) -> Result<Vec<AcceptedCoverage>, OrbitError> {
        self.with_read_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT receipt_json FROM automation_coverage WHERE consumer=?1 ORDER BY accepted_at DESC,batch_id DESC LIMIT ?2",
                )
                .map_err(|e| OrbitError::Store(e.to_string()))?;

            let rows = stmt
                .query_map(params![consumer, limit.min(100)], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|e| OrbitError::Store(e.to_string()))?;

            rows.map(|row| decode(&row.map_err(|e| OrbitError::Store(e.to_string()))?))
                .collect()
        })
    }
}
