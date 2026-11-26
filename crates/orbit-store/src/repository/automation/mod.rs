//! Concrete repository for host automation records.

mod checkpoint;
pub(super) mod codec;

use crate::store::{Snapshot, Tx};
use codec::{decode, encode};
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::automation::{
    AcceptedCoverage, AutomationState, BatchAttempt, BatchWaiver, Delivery,
    recovery::RecoveryRecord,
};
use rusqlite::Connection;
use rusqlite::{OptionalExtension, params};
use std::marker::PhantomData;

/// Host automation records scoped to a snapshot or write transaction.
pub struct AutomationRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}
impl<'a, 's> AutomationRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}
impl<'a, 't> AutomationRepository<'a, Tx<'t>> {
    pub(crate) fn for_tx(tx: &'a Tx<'t>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }
}

impl<Access> AutomationRepository<'_, Access> {
    pub fn receipt(
        &self,
        consumer: &str,
        batch: &str,
    ) -> Result<Option<AcceptedCoverage>, OrbitError> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT receipt_json FROM automation_coverage WHERE consumer=?1 AND batch_id=?2",
                params![consumer, batch],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_store_error)?;
        raw.as_deref().map(decode).transpose()
    }

    pub fn receipt_count(&self, consumer: &str) -> Result<usize, OrbitError> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM automation_coverage WHERE consumer=?1",
                [consumer],
                |row| row.get(0),
            )
            .map_err(sqlite_store_error)?;
        usize::try_from(count)
            .map_err(|_| OrbitError::Store("invalid accepted coverage count".into()))
    }

    pub fn state(&self, consumer: &str) -> Result<Option<AutomationState>, OrbitError> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT state_json FROM automation_consumers WHERE consumer=?1",
                [consumer],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_store_error)?;
        raw.as_deref().map(decode).transpose()
    }

    pub fn states_page(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<AutomationState>, OrbitError> {
        if limit == 0 {
            return Err(OrbitError::InvalidInput(
                "automation state page limit must be nonzero".into(),
            ));
        }
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT consumer,state_json FROM automation_consumers \
                     WHERE consumer >= ?1 AND consumer > ?2 \
                     AND substr(consumer,1,length(?1)) = ?1 \
                     ORDER BY consumer LIMIT ?3",
                )
                .map_err(sqlite_store_error)?;
            let rows = stmt
                .query_map(
                    params![prefix, after.unwrap_or(""), limit.min(100)],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(sqlite_store_error)?;
            rows.map(|row| {
                let (consumer, raw) = row.map_err(sqlite_store_error)?;
                let state: AutomationState = decode(&raw)?;
                if state.consumer != consumer {
                    return Err(OrbitError::Store(
                        "persisted automation consumer key does not match its state".into(),
                    ));
                }
                Ok(state)
            })
            .collect()
        }
    }

    pub fn receipts(
        &self,
        consumer: &str,
        limit: usize,
    ) -> Result<Vec<AcceptedCoverage>, OrbitError> {
        let mut stmt = self.conn
                .prepare(
                    "SELECT receipt_json FROM automation_coverage WHERE consumer=?1 ORDER BY accepted_at DESC,batch_id DESC LIMIT ?2",
                )
                .map_err(sqlite_store_error)?;

        let rows = stmt
            .query_map(params![consumer, limit.min(100)], |row| {
                row.get::<_, String>(0)
            })
            .map_err(sqlite_store_error)?;

        rows.map(|row| decode(&row.map_err(sqlite_store_error)?))
            .collect()
    }

    pub fn waivers(&self, consumer: &str, limit: usize) -> Result<Vec<BatchWaiver>, OrbitError> {
        let mut stmt = self.conn
            .prepare(
                "SELECT waiver_json FROM automation_waivers WHERE consumer=?1 ORDER BY rowid DESC LIMIT ?2",
            )
            .map_err(sqlite_store_error)?;

        let rows = stmt
            .query_map(params![consumer, limit.min(100)], |row| {
                row.get::<_, String>(0)
            })
            .map_err(sqlite_store_error)?;

        rows.map(|row| decode(&row.map_err(sqlite_store_error)?))
            .collect()
    }

    pub fn waiver(&self, consumer: &str, batch: &str) -> Result<Option<BatchWaiver>, OrbitError> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT waiver_json FROM automation_waivers WHERE consumer=?1 AND batch_id=?2",
                params![consumer, batch],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_store_error)?;
        raw.as_deref().map(decode).transpose()
    }

    pub fn recoveries(
        &self,
        consumer: &str,
        limit: usize,
    ) -> Result<Vec<RecoveryRecord>, OrbitError> {
        let mut stmt = self.conn
            .prepare(
                "SELECT record_json FROM automation_recoveries WHERE consumer=?1 ORDER BY recorded_at DESC,rowid DESC LIMIT ?2",
            )
            .map_err(sqlite_store_error)?;

        let rows = stmt
            .query_map(params![consumer, limit.min(100)], |row| {
                row.get::<_, String>(0)
            })
            .map_err(sqlite_store_error)?;

        rows.map(|row| decode(&row.map_err(sqlite_store_error)?))
            .collect()
    }

    pub fn delivery_intents(
        &self,
        repository: &str,
        branch: &str,
        commits: &[String],
    ) -> Result<Vec<Delivery>, OrbitError> {
        if commits.len() > 200 {
            return Err(OrbitError::InvalidInput(
                "delivery lookup exceeds page limit".into(),
            ));
        }
        {
            // Keyed by record id so a delivery spanning several requested commits is
            // returned once.
            let mut found = std::collections::BTreeMap::new();

            let mut stmt = self.conn
            .prepare(
                "SELECT i.record_id,i.delivery_json FROM automation_delivery_members m JOIN automation_delivery_intents i ON i.record_id=m.record_id WHERE m.repository=?1 AND m.branch=?2 AND m.commit_id=?3 LIMIT 51",
            )
            .map_err(sqlite_store_error)?;

            for sha in commits {
                let rows = stmt
                    .query_map(params![repository, branch, sha], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .map_err(sqlite_store_error)?;

                for row in rows {
                    let (id, raw) = row.map_err(sqlite_store_error)?;
                    found.insert(id, decode(&raw)?);

                    if found.len() > 50 {
                        return Err(OrbitError::Store(
                            "delivery intent page exceeds limit".into(),
                        ));
                    }
                }
            }

            Ok(found.into_values().collect())
        }
    }

    pub fn delivery_intent(&self, delivery: &Delivery) -> Result<Option<Delivery>, OrbitError> {
        let id = record_id(delivery);
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT delivery_json FROM automation_delivery_intents WHERE record_id=?1",
                [&id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_store_error)?;
        raw.as_deref().map(decode).transpose()
    }
}
impl AutomationRepository<'_, Tx<'_>> {
    pub fn insert_state(&self, state: &AutomationState) -> Result<bool, OrbitError> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO automation_consumers VALUES (?1,0,?2)",
                params![state.consumer, encode(state)?],
            )
            .map(|n| n == 1)
            .map_err(sqlite_store_error)
    }

    /// Replace a checkpoint only if the decoded prior row still matches.
    pub fn replace_state(
        &self,
        previous: &AutomationState,
        next: &AutomationState,
    ) -> Result<bool, OrbitError> {
        checkpoint::advance(self.conn, previous, next)
    }

    /// Delete a checkpoint only if the decoded prior row still matches.
    pub fn delete_state(&self, previous: &AutomationState) -> Result<bool, OrbitError> {
        let Some(raw) = checkpoint::previous_json(self.conn, previous)? else {
            return Ok(false);
        };
        self.conn.execute(
            "DELETE FROM automation_consumers WHERE consumer=?1 AND generation=?2 AND state_json=?3",
            params![previous.consumer, previous.generation, raw],
        ).map(|changed| changed != 0).map_err(sqlite_store_error)
    }

    pub fn insert_receipt(
        &self,
        previous: &AutomationState,
        receipt: &AcceptedCoverage,
    ) -> Result<(), OrbitError> {
        let conn = self.conn;
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
        .map_err(sqlite_store_error)?;
        Ok(())
    }

    pub fn insert_waiver(
        &self,
        consumer: &str,
        active: &BatchAttempt,
        waiver: &BatchWaiver,
    ) -> Result<(), OrbitError> {
        let conn = self.conn;
        conn.execute(
            "INSERT INTO automation_waivers VALUES (?1,?2,?3,?4)",
            params![waiver.batch_id, consumer, encode(active)?, encode(waiver)?],
        )
        .map_err(sqlite_store_error)?;

        Ok(())
    }

    pub fn insert_recovery(
        &self,
        consumer: &str,
        record: &RecoveryRecord,
    ) -> Result<(), OrbitError> {
        let conn = self.conn;

        let record_json = encode(record)?;
        let record_id = sha256_hex(record_json.as_bytes());

        conn.execute(
            "INSERT INTO automation_recoveries VALUES (?1,?2,?3,?4)",
            params![record_id, consumer, record.at.to_rfc3339(), record_json],
        )
        .map_err(sqlite_store_error)?;

        Ok(())
    }

    pub fn insert_delivery_intent(&self, delivery: &Delivery) -> Result<(), OrbitError> {
        let conn = self.conn;
        let id = record_id(delivery);
        conn.execute(
            "INSERT INTO automation_delivery_intents VALUES (?1,?2,?3,?4)",
            params![id, delivery.repository, delivery.branch, encode(delivery)?],
        )
        .map_err(sqlite_store_error)?;

        for sha in &delivery.commits {
            conn.execute(
                "INSERT INTO automation_delivery_members VALUES (?1,?2,?3,?4)",
                params![delivery.repository, delivery.branch, sha, id],
            )
            .map_err(sqlite_store_error)?;
        }

        Ok(())
    }
}
fn record_id(delivery: &Delivery) -> String {
    format!("{}:{}", delivery.key, delivery.after.commit)
}
