//! Host-local routine circuit breakers and their transition log [ORB-25103].

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{OptionalExtension, Row, Transaction, TransactionBehavior, params};

use super::{RoutineBreakerEvent, RoutineBreakerRecord, RoutineBreakerState};
use crate::Store;

const BREAKER_COLUMNS: &str = "routine_name, source_workspace, state, error_class, last_run_id, \
     tripped_at, trips, next_trial_at, orbit_build, trial_started_at, trial_reason, \
     bypass_requested_at, floor_at, updated_at";

impl Store {
    /// Every routine breaker row on this host, keyed by routine name.
    pub fn routine_breakers(&self) -> Result<BTreeMap<String, RoutineBreakerRecord>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(&format!("SELECT {BREAKER_COLUMNS} FROM routine_breakers"))
            .map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map([], breaker_from_row)
            .map_err(sqlite_store_error)?;
        let mut breakers = BTreeMap::new();
        for row in rows {
            let breaker = row.map_err(sqlite_store_error)??;
            breakers.insert(breaker.routine_name.clone(), breaker);
        }
        Ok(breakers)
    }

    /// Write `breaker` and append `event` in one transaction.
    pub fn routine_breaker_transition(
        &self,
        breaker: &RoutineBreakerRecord,
        event: &RoutineBreakerEvent,
    ) -> Result<(), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            upsert_breaker(&tx.tx, breaker)?;
            insert_event(&tx.tx, event)
        })
    }

    /// Close `routine_name`'s breaker now and record the reset. Fires before
    /// now no longer count toward a trip. Returns `false` when it was not open.
    pub fn routine_breaker_reset(
        &self,
        routine_name: &str,
        actor: &str,
    ) -> Result<bool, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let Some(mut breaker) = select_breaker(&tx.tx, routine_name)? else {
                return Ok(false);
            };
            if breaker.state != RoutineBreakerState::Open {
                return Ok(false);
            }
            let now = crate::now_string();
            let detail = breaker.error_class.clone();
            breaker.state = RoutineBreakerState::Closed;
            breaker.trips = 0;
            breaker.next_trial_at = None;
            breaker.trial_started_at = None;
            breaker.trial_reason = None;
            breaker.bypass_requested_at = None;
            breaker.floor_at = Some(now.clone());
            breaker.updated_at = now.clone();
            upsert_breaker(&tx.tx, &breaker)?;
            insert_event(
                &tx.tx,
                &RoutineBreakerEvent {
                    routine_name: routine_name.to_string(),
                    event: "reset".into(),
                    actor: actor.to_string(),
                    detail,
                    run_id: None,
                    at: now,
                },
            )?;
            Ok(true)
        })
    }

    /// Ask the sweep to let one fire of `routine_name` through on its next
    /// pass, and record the request. Returns `false` when the breaker is not
    /// open.
    pub fn routine_breaker_bypass(
        &self,
        routine_name: &str,
        actor: &str,
    ) -> Result<bool, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let Some(mut breaker) = select_breaker(&tx.tx, routine_name)? else {
                return Ok(false);
            };
            if breaker.state != RoutineBreakerState::Open {
                return Ok(false);
            }
            let now = crate::now_string();
            breaker.bypass_requested_at = Some(now.clone());
            breaker.updated_at = now.clone();
            upsert_breaker(&tx.tx, &breaker)?;
            insert_event(
                &tx.tx,
                &RoutineBreakerEvent {
                    routine_name: routine_name.to_string(),
                    event: "bypass".into(),
                    actor: actor.to_string(),
                    detail: breaker.error_class.clone(),
                    run_id: None,
                    at: now,
                },
            )?;
            Ok(true)
        })
    }

    /// Newest breaker events for one routine, newest first.
    pub fn routine_breaker_events(
        &self,
        routine_name: &str,
        limit: usize,
    ) -> Result<Vec<RoutineBreakerEvent>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT routine_name, event, actor, detail, run_id, at
                 FROM routine_breaker_events WHERE routine_name = ?1
                 ORDER BY id DESC LIMIT ?2",
            )
            .map_err(sqlite_store_error)?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt
            .query_map(params![routine_name, limit], |row| {
                Ok(RoutineBreakerEvent {
                    routine_name: row.get(0)?,
                    event: row.get(1)?,
                    actor: row.get(2)?,
                    detail: row.get(3)?,
                    run_id: row.get(4)?,
                    at: row.get(5)?,
                })
            })
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }
}

fn select_breaker(
    tx: &Transaction<'_>,
    routine_name: &str,
) -> Result<Option<RoutineBreakerRecord>, OrbitError> {
    tx.query_row(
        &format!("SELECT {BREAKER_COLUMNS} FROM routine_breakers WHERE routine_name = ?1"),
        params![routine_name],
        breaker_from_row,
    )
    .optional()
    .map_err(sqlite_store_error)?
    .transpose()
}

fn upsert_breaker(tx: &Transaction<'_>, breaker: &RoutineBreakerRecord) -> Result<(), OrbitError> {
    tx.execute(
        &format!(
            "INSERT OR REPLACE INTO routine_breakers ({BREAKER_COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
        ),
        params![
            breaker.routine_name,
            breaker.source_workspace,
            breaker.state.as_str(),
            breaker.error_class,
            breaker.last_run_id,
            breaker.tripped_at,
            breaker.trips,
            breaker.next_trial_at,
            breaker.orbit_build,
            breaker.trial_started_at,
            breaker.trial_reason,
            breaker.bypass_requested_at,
            breaker.floor_at,
            breaker.updated_at,
        ],
    )
    .map_err(sqlite_store_error)?;
    Ok(())
}

fn insert_event(tx: &Transaction<'_>, event: &RoutineBreakerEvent) -> Result<(), OrbitError> {
    tx.execute(
        "INSERT INTO routine_breaker_events (routine_name, event, actor, detail, run_id, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            event.routine_name,
            event.event,
            event.actor,
            event.detail,
            event.run_id,
            event.at,
        ],
    )
    .map_err(sqlite_store_error)?;
    Ok(())
}

fn breaker_from_row(row: &Row<'_>) -> rusqlite::Result<Result<RoutineBreakerRecord, OrbitError>> {
    let state: String = row.get(2)?;
    let Some(state) = RoutineBreakerState::parse(&state) else {
        return Ok(Err(OrbitError::Store(format!(
            "unknown routine breaker state '{state}'"
        ))));
    };
    Ok(Ok(RoutineBreakerRecord {
        routine_name: row.get(0)?,
        source_workspace: row.get(1)?,
        state,
        error_class: row.get(3)?,
        last_run_id: row.get(4)?,
        tripped_at: row.get(5)?,
        trips: row.get(6)?,
        next_trial_at: row.get(7)?,
        orbit_build: row.get(8)?,
        trial_started_at: row.get(9)?,
        trial_reason: row.get(10)?,
        bypass_requested_at: row.get(11)?,
        floor_at: row.get(12)?,
        updated_at: row.get(13)?,
    }))
}
