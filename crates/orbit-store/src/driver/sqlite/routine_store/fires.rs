//! Fire intent, dispatch and outcome writes, fire queries, and row hydration.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{RoutineFireIntentParams, RoutineFireRecord, RoutineFireState, RoutineSlotAttempts};
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
                .map_err(sqlite_store_error)?;
            if inserted == 0 {
                return Ok(false);
            }
            // Slots are chrono RFC3339 UTC (`+00:00`), so the text order is
            // chronological. A retry of an older slot must not rewind a cursor
            // that a newer skipped slot already advanced.
            tx.tx
                .execute(
                    "UPDATE routine_cursors SET last_slot = ?2, updated_at = ?3
                     WHERE routine_name = ?1
                       AND (last_slot IS NULL OR last_slot < ?2)",
                    params![intent.routine_name, intent.slot, now],
                )
                .map_err(sqlite_store_error)?;
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
                .map_err(sqlite_store_error)?;
            Ok(())
        })
    }

    /// Most recent fire attempt for one routine, if any.
    pub fn routine_latest_fire(
        &self,
        routine_name: &str,
    ) -> Result<Option<RoutineFireRecord>, OrbitError> {
        self.routine_one_fire(
            &format!(
                "SELECT {FIRE_COLUMNS} FROM routine_fires
                 WHERE routine_name = ?1
                 ORDER BY slot DESC, attempt DESC LIMIT 1"
            ),
            routine_name,
        )
    }

    /// Newest intent or dispatched fire for one routine, if work is in flight.
    ///
    /// `overlap: forbid` uses this rather than [`Self::routine_latest_fire`]:
    /// a later skipped slot is terminal and must not hide an older run.
    pub fn routine_blocking_fire(
        &self,
        routine_name: &str,
    ) -> Result<Option<RoutineFireRecord>, OrbitError> {
        self.routine_one_fire(
            &format!(
                "SELECT {FIRE_COLUMNS} FROM routine_fires
                 WHERE routine_name = ?1 AND state IN ('intent', 'dispatched')
                 ORDER BY slot DESC, attempt DESC LIMIT 1"
            ),
            routine_name,
        )
    }

    /// Newest fire that is not a skipped slot. Retry looks here so a later
    /// `missed_run: skip` row cannot bury a failed attempt that still has budget.
    pub fn routine_latest_non_skip_fire(
        &self,
        routine_name: &str,
    ) -> Result<Option<RoutineFireRecord>, OrbitError> {
        self.routine_one_fire(
            &format!(
                "SELECT {FIRE_COLUMNS} FROM routine_fires
                 WHERE routine_name = ?1 AND state != 'skipped'
                 ORDER BY slot DESC, attempt DESC LIMIT 1"
            ),
            routine_name,
        )
    }

    /// Highest attempt number on one slot, and how many of those rows are
    /// real tries. `None` when the slot has no fires.
    ///
    /// A drain-skipped retry occupies `highest` and is excluded from
    /// `non_skipped`, so retry selection can step past it without spending
    /// `retries.max`.
    pub fn routine_slot_attempts(
        &self,
        routine_name: &str,
        slot: &str,
    ) -> Result<Option<RoutineSlotAttempts>, OrbitError> {
        let conn = self.read()?;
        let (highest, non_skipped) = conn
            .query_row(
                "SELECT MAX(attempt),
                        COALESCE(SUM(CASE WHEN state != 'skipped' THEN 1 ELSE 0 END), 0)
                 FROM routine_fires
                 WHERE routine_name = ?1 AND slot = ?2",
                params![routine_name, slot],
                |row| {
                    let highest: Option<i64> = row.get(0)?;
                    let non_skipped: i64 = row.get(1)?;
                    Ok((highest, non_skipped))
                },
            )
            .map_err(sqlite_store_error)?;
        let Some(highest) = highest else {
            return Ok(None);
        };
        Ok(Some(RoutineSlotAttempts {
            highest: sql_attempt(highest)?,
            non_skipped: sql_attempt(non_skipped)?,
        }))
    }

    /// Newest successful fire. Freshness uses `updated_at`, so a catch-up slot
    /// that just succeeded counts as recent even when its scheduled slot is old.
    pub fn routine_latest_succeeded_fire(
        &self,
        routine_name: &str,
    ) -> Result<Option<RoutineFireRecord>, OrbitError> {
        self.routine_one_fire(
            &format!(
                "SELECT {FIRE_COLUMNS} FROM routine_fires
                 WHERE routine_name = ?1 AND state = 'succeeded'
                 ORDER BY updated_at DESC, slot DESC, attempt DESC LIMIT 1"
            ),
            routine_name,
        )
    }

    fn routine_one_fire(
        &self,
        sql: &str,
        routine_name: &str,
    ) -> Result<Option<RoutineFireRecord>, OrbitError> {
        let conn = self.read()?;
        conn.query_row(sql, params![routine_name], fire_row)
            .optional()
            .map_err(sqlite_store_error)?
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
            .map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(
                params![
                    RoutineFireState::Intent.as_str(),
                    RoutineFireState::Dispatched.as_str()
                ],
                fire_row,
            )
            .map_err(sqlite_store_error)?;
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
            .map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(params![routine_name, limit as i64], fire_row)
            .map_err(sqlite_store_error)?;
        collect_fires(rows)
    }
}

const FIRE_COLUMNS: &str =
    "routine_name, slot, attempt, state, run_id, source_workspace, detail, created_at, updated_at";

fn sql_attempt(value: i64) -> Result<u32, OrbitError> {
    u32::try_from(value).map_err(|_| {
        OrbitError::Store(format!(
            "routine fire attempt {value} is outside the supported range"
        ))
    })
}

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
            row.map_err(sqlite_store_error)?,
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
