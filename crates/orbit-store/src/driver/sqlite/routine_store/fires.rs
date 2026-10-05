//! Fire intent, dispatch and outcome writes, fire queries, and row hydration.

use orbit_common::OrbitError;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{RoutineFireIntentParams, RoutineFireRecord, RoutineFireState};
use crate::Store;

impl Store {
    /// Record the intent to fire one (routine, slot, attempt) and advance the
    /// cursor's `last_slot` in the same transaction. Returns `false` when the
    /// idempotency key already exists — the slot was already claimed by an
    /// earlier sweep (possibly one that crashed mid-dispatch), and the caller
    /// must not dispatch again.
    pub fn routine_record_fire_intent(
        &self,
        intent: &RoutineFireIntentParams,
    ) -> Result<bool, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let inserted = tx
                .tx
                .execute(
                    "INSERT OR IGNORE INTO routine_fires
                     (routine_name, slot, attempt, state, run_id, source_workspace,
                      detail, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, NULL, ?5, NULL, ?6, ?6)",
                    params![
                        intent.routine_name,
                        intent.slot,
                        intent.attempt,
                        RoutineFireState::Intent.as_str(),
                        intent.source_workspace,
                        now,
                    ],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if inserted == 0 {
                return Ok(false);
            }
            tx.tx
                .execute(
                    "UPDATE routine_cursors SET last_slot = ?2, updated_at = ?3
                     WHERE routine_name = ?1",
                    params![intent.routine_name, intent.slot, now],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(true)
        })
    }

    /// Mark a recorded fire intent as dispatched, attaching the run id the
    /// pipeline submission returned.
    pub fn routine_mark_fire_dispatched(
        &self,
        routine_name: &str,
        slot: &str,
        attempt: u32,
        run_id: &str,
    ) -> Result<(), OrbitError> {
        self.routine_update_fire(
            routine_name,
            slot,
            attempt,
            RoutineFireState::Dispatched,
            Some(run_id),
            None,
        )
    }

    /// Record the terminal outcome of a fire (succeeded / failed / timed out
    /// / error), with an optional human-readable detail message.
    pub fn routine_mark_fire_outcome(
        &self,
        routine_name: &str,
        slot: &str,
        attempt: u32,
        state: RoutineFireState,
        detail: Option<&str>,
    ) -> Result<(), OrbitError> {
        self.routine_update_fire(routine_name, slot, attempt, state, None, detail)
    }

    fn routine_update_fire(
        &self,
        routine_name: &str,
        slot: &str,
        attempt: u32,
        state: RoutineFireState,
        run_id: Option<&str>,
        detail: Option<&str>,
    ) -> Result<(), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            tx.tx
                .execute(
                    "UPDATE routine_fires
                     SET state = ?4,
                         run_id = COALESCE(?5, run_id),
                         detail = COALESCE(?6, detail),
                         updated_at = ?7
                     WHERE routine_name = ?1 AND slot = ?2 AND attempt = ?3",
                    params![
                        routine_name,
                        slot,
                        attempt,
                        state.as_str(),
                        run_id,
                        detail,
                        now
                    ],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(())
        })
    }

    /// Most recent fire attempt for one routine, if any.
    pub fn routine_latest_fire(
        &self,
        routine_name: &str,
    ) -> Result<Option<RoutineFireRecord>, OrbitError> {
        let conn = self.read()?;
        conn.query_row(
            &format!(
                "SELECT {FIRE_COLUMNS} FROM routine_fires
                 WHERE routine_name = ?1
                 ORDER BY slot DESC, attempt DESC LIMIT 1"
            ),
            params![routine_name],
            fire_row,
        )
        .optional()
        .map_err(|error| OrbitError::Store(error.to_string()))?
        .map(RoutineFireRecord::try_from_row)
        .transpose()
    }

    /// All fires that have not reached a terminal state (intent recorded but
    /// never dispatched, or dispatched with the run outcome not yet synced).
    pub fn routine_unresolved_fires(&self) -> Result<Vec<RoutineFireRecord>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {FIRE_COLUMNS} FROM routine_fires
                 WHERE state IN (?1, ?2)
                 ORDER BY routine_name ASC, slot ASC, attempt ASC"
            ))
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let rows = stmt
            .query_map(
                params![
                    RoutineFireState::Intent.as_str(),
                    RoutineFireState::Dispatched.as_str()
                ],
                fire_row,
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        collect_fires(rows)
    }

    /// Recent fire attempts for one routine, newest first.
    pub fn routine_recent_fires(
        &self,
        routine_name: &str,
        limit: usize,
    ) -> Result<Vec<RoutineFireRecord>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT {FIRE_COLUMNS} FROM routine_fires
                 WHERE routine_name = ?1
                 ORDER BY slot DESC, attempt DESC LIMIT ?2"
            ))
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let rows = stmt
            .query_map(params![routine_name, limit as i64], fire_row)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        collect_fires(rows)
    }
}

const FIRE_COLUMNS: &str =
    "routine_name, slot, attempt, state, run_id, source_workspace, detail, created_at, updated_at";

struct FireRow {
    routine_name: String,
    slot: String,
    attempt: u32,
    state: String,
    run_id: Option<String>,
    source_workspace: String,
    detail: Option<String>,
    created_at: String,
    updated_at: String,
}

fn fire_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FireRow> {
    Ok(FireRow {
        routine_name: row.get(0)?,
        slot: row.get(1)?,
        attempt: row.get(2)?,
        state: row.get(3)?,
        run_id: row.get(4)?,
        source_workspace: row.get(5)?,
        detail: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn collect_fires(
    rows: impl Iterator<Item = rusqlite::Result<FireRow>>,
) -> Result<Vec<RoutineFireRecord>, OrbitError> {
    let mut fires = Vec::new();
    for row in rows {
        fires.push(RoutineFireRecord::try_from_row(
            row.map_err(|error| OrbitError::Store(error.to_string()))?,
        )?);
    }
    Ok(fires)
}

impl RoutineFireRecord {
    fn try_from_row(row: FireRow) -> Result<Self, OrbitError> {
        Ok(Self {
            state: RoutineFireState::parse(&row.state).ok_or_else(|| {
                OrbitError::Store(format!(
                    "routine fire ({}, {}, {}) has unknown state '{}'",
                    row.routine_name, row.slot, row.attempt, row.state
                ))
            })?,
            routine_name: row.routine_name,
            slot: row.slot,
            attempt: row.attempt,
            run_id: row.run_id,
            source_workspace: row.source_workspace,
            detail: row.detail,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}
