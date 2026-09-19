//! Host-local scheduler state for the routines feature [ORB-10021].
//!
//! All rows live in the host-global store database (`~/.orbit/orbit.db`) and
//! are never synced between hosts (ADR-0208): routine *definitions* converge
//! via git; fires, pauses, and the sweep lock stay local. Three tables:
//!
//! - `routine_cursors` — per routine: when this host first observed it
//!   (the baseline; a routine never fires for slots that predate its first
//!   observation) and the last scheduled slot consumed.
//! - `routine_fires` — one row per fire attempt, keyed on the idempotency
//!   triple (routine name, scheduled slot, attempt). Recording the intent
//!   and advancing the cursor happen in one transaction, so a slot can
//!   never double-fire even across crashed sweeps.
//! - `routine_pauses` — host-local suppressions written by
//!   `orbit routine pause`, invisible to git.
//!
//! The sweep advisory lock is a `flock(2)` file lock, not a table: the OS
//! releases it on process death, so a crashed sweep never wedges the next
//! one (see `try_acquire_routine_sweep_lock`).

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::Store;
use crate::fs::lock::{self, FileLockGuard};

pub use crate::contracts::{
    RoutineCursor, RoutineFireIntentParams, RoutineFireRecord, RoutineFireState, RoutinePauseRecord,
};

/// Lock file name under the global state dir guarding one sweep pass per host.
const SWEEP_LOCK_FILE: &str = "routine-sweep.lock";

/// RAII guard over the host-global sweep advisory lock
/// (`docs/design-patterns/raii_guard.md`): dropping it releases the lock.
#[derive(Debug)]
#[must_use = "the sweep lock is released as soon as the guard is dropped"]
pub struct RoutineSweepLock {
    _guard: FileLockGuard,
}

/// Try to take this host's sweep lock without queueing behind a live holder.
/// `Ok(None)` means another sweep pass is in flight and the caller should exit
/// cleanly — overlapping invocations from a slow prior pass must not
/// double-fire. A refusal no holder claims is not such a pass; see
/// `orbit_common::fs::file_lock::try_acquire_exclusive_file_lock`.
pub fn try_acquire_routine_sweep_lock(
    global_state_dir: &Path,
) -> Result<Option<RoutineSweepLock>, OrbitError> {
    let path = global_state_dir.join(SWEEP_LOCK_FILE);
    Ok(lock::try_acquire_exclusive(&path, "routine sweep")?
        .map(|guard| RoutineSweepLock { _guard: guard }))
}

impl Store {
    /// Cursor for one routine, if this host has observed it before.
    pub fn routine_cursor(&self, routine_name: &str) -> Result<Option<RoutineCursor>, OrbitError> {
        let conn = self.read()?;
        conn.query_row(
            "SELECT routine_name, baseline_at, last_slot FROM routine_cursors
             WHERE routine_name = ?1",
            params![routine_name],
            |row| {
                Ok(RoutineCursor {
                    routine_name: row.get(0)?,
                    baseline_at: row.get(1)?,
                    last_slot: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(|error| OrbitError::Store(error.to_string()))
    }

    /// Record the first observation of a routine on this host. Idempotent:
    /// an existing cursor is left untouched, so the baseline never moves
    /// backwards or forwards once set. Returns whether a row was created.
    pub fn routine_record_baseline(
        &self,
        routine_name: &str,
        baseline_at: &str,
    ) -> Result<bool, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let inserted = tx
                .tx
                .execute(
                    "INSERT OR IGNORE INTO routine_cursors
                     (routine_name, baseline_at, last_slot, updated_at)
                     VALUES (?1, ?2, NULL, ?3)",
                    params![routine_name, baseline_at, now],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(inserted > 0)
        })
    }

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

    /// Suppress a routine on this host. Returns `false` when already paused.
    pub fn routine_pause(&self, routine_name: &str, actor: &str) -> Result<bool, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let inserted = tx
                .tx
                .execute(
                    "INSERT OR IGNORE INTO routine_pauses (routine_name, paused_at, actor)
                     VALUES (?1, ?2, ?3)",
                    params![routine_name, now, actor],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(inserted > 0)
        })
    }

    /// Clear a host-local pause. Returns `false` when it was not paused.
    pub fn routine_resume(&self, routine_name: &str) -> Result<bool, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let deleted = tx
                .tx
                .execute(
                    "DELETE FROM routine_pauses WHERE routine_name = ?1",
                    params![routine_name],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(deleted > 0)
        })
    }

    /// All host-local pauses, keyed by routine name.
    pub fn routine_pauses(&self) -> Result<BTreeMap<String, RoutinePauseRecord>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare("SELECT routine_name, paused_at, actor FROM routine_pauses")
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(RoutinePauseRecord {
                    routine_name: row.get(0)?,
                    paused_at: row.get(1)?,
                    actor: row.get(2)?,
                })
            })
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let mut pauses = BTreeMap::new();
        for row in rows {
            let pause = row.map_err(|error| OrbitError::Store(error.to_string()))?;
            pauses.insert(pause.routine_name.clone(), pause);
        }
        Ok(pauses)
    }
}

impl crate::contracts::RoutineStoreBackend for Store {
    fn routine_cursor(&self, routine_name: &str) -> Result<Option<RoutineCursor>, OrbitError> {
        Self::routine_cursor(self, routine_name)
    }

    fn routine_record_baseline(
        &self,
        routine_name: &str,
        baseline_at: &str,
    ) -> Result<bool, OrbitError> {
        Self::routine_record_baseline(self, routine_name, baseline_at)
    }

    fn routine_record_fire_intent(
        &self,
        intent: &RoutineFireIntentParams,
    ) -> Result<bool, OrbitError> {
        Self::routine_record_fire_intent(self, intent)
    }

    fn routine_mark_fire_dispatched(
        &self,
        routine_name: &str,
        slot: &str,
        attempt: u32,
        run_id: &str,
    ) -> Result<(), OrbitError> {
        Self::routine_mark_fire_dispatched(self, routine_name, slot, attempt, run_id)
    }

    fn routine_mark_fire_outcome(
        &self,
        routine_name: &str,
        slot: &str,
        attempt: u32,
        state: RoutineFireState,
        detail: Option<&str>,
    ) -> Result<(), OrbitError> {
        Self::routine_mark_fire_outcome(self, routine_name, slot, attempt, state, detail)
    }

    fn routine_latest_fire(
        &self,
        routine_name: &str,
    ) -> Result<Option<RoutineFireRecord>, OrbitError> {
        Self::routine_latest_fire(self, routine_name)
    }

    fn routine_unresolved_fires(&self) -> Result<Vec<RoutineFireRecord>, OrbitError> {
        Self::routine_unresolved_fires(self)
    }

    fn routine_recent_fires(
        &self,
        routine_name: &str,
        limit: usize,
    ) -> Result<Vec<RoutineFireRecord>, OrbitError> {
        Self::routine_recent_fires(self, routine_name, limit)
    }

    fn routine_pause(&self, routine_name: &str, actor: &str) -> Result<bool, OrbitError> {
        Self::routine_pause(self, routine_name, actor)
    }

    fn routine_resume(&self, routine_name: &str) -> Result<bool, OrbitError> {
        Self::routine_resume(self, routine_name)
    }

    fn routine_pauses(&self) -> Result<BTreeMap<String, RoutinePauseRecord>, OrbitError> {
        Self::routine_pauses(self)
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

#[cfg(test)]
mod tests;
