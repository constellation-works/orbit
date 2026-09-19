use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{Duration, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::path::workspace_relative_paths_overlap;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::{
    ActiveTaskReservation, ExpiredTaskReservation, ReleasedTaskReservation, Store,
    TaskLockConflict, TaskLockHolder, TaskReservationCheckParams, TaskReservationCheckResult,
    TaskReservationListResult, TaskReservationOwnedConflictsParams,
    TaskReservationOwnedConflictsResult, TaskReservationReleaseByOwnerParams,
    TaskReservationReleaseByOwnerResult, TaskReservationReleaseParams,
    TaskReservationReleaseReason, TaskReservationReleaseResult, TaskReservationReserveParams,
    TaskReservationReserveResult, TaskReservationScope,
};

mod workspace_claim;

static ROW_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Mint a reservation or claim row id that cannot collide with another
/// caller's. The wall clock alone is not enough: two agents reserving in the
/// same nanosecond tick (or on a clock that fails and reads as 0) would both
/// produce the same primary key and one would abort on the UNIQUE constraint
/// instead of getting a reservation. The process id separates processes and
/// the sequence separates threads within one.
pub(super) fn unique_row_id(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = ROW_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}{nanos}-{}-{sequence}", std::process::id())
}

impl Store {
    /// Inspect active reservations through a pooled read connection. Unlike
    /// the operational list/check paths, this does not opportunistically mark
    /// expired rows released, which keeps `orbit doctor` strictly read-only.
    pub fn inspect_active_task_reservations(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<Vec<ActiveTaskReservation>, OrbitError> {
        self.with_read_connection(|conn| {
            let now = crate::now_string();
            let sql = format!(
                "SELECT {}
                 FROM task_reservations
                 WHERE {}
                   AND released_at IS NULL
                   AND expires_at > ?3
                 ORDER BY created_at ASC, reservation_id ASC",
                select_reservation_columns(),
                reservation_scope_clause(TaskReservationScope::Files),
            );
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            let rows = stmt
                .query_map(
                    params![workspace_id, workspace_orbit_dir, now],
                    reservation_row,
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;

            let mut reservations = Vec::new();
            for row in rows {
                reservations.push(
                    row.map_err(|error| OrbitError::Store(error.to_string()))?
                        .into_active()?,
                );
            }
            Ok(reservations)
        })
    }

    pub fn list_active_task_reservations(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<TaskReservationListResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let expired_reservations = expire_reservations_in_scope(
                tx,
                workspace_orbit_dir,
                workspace_id,
                &now,
                TaskReservationScope::Files,
            )?;
            let sql = format!(
                "SELECT {}
                 FROM task_reservations
                 WHERE {}
                   AND released_at IS NULL
                   AND expires_at > ?3
                 ORDER BY created_at ASC, reservation_id ASC",
                select_reservation_columns(),
                reservation_scope_clause(TaskReservationScope::Files),
            );
            let mut stmt = tx
                .tx
                .prepare(&sql)
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            let rows = stmt
                .query_map(
                    params![workspace_id, workspace_orbit_dir, now],
                    reservation_row,
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;

            let mut reservations = Vec::new();
            for row in rows {
                reservations.push(
                    row.map_err(|error| OrbitError::Store(error.to_string()))?
                        .into_active()?,
                );
            }

            Ok(TaskReservationListResult {
                reservations,
                expired_reservations,
            })
        })
    }

    pub fn check_task_reservation_conflicts(
        &self,
        params: &TaskReservationCheckParams,
    ) -> Result<TaskReservationCheckResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let expired_reservations = expire_reservations_in_scope(
                tx,
                &params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
                &now,
                TaskReservationScope::Files,
            )?;
            let conflicts = find_reservation_conflicts(
                tx,
                &params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
                &now,
                &params.requested_files,
            )?;
            Ok(TaskReservationCheckResult {
                conflicts,
                expired_reservations,
            })
        })
    }

    pub fn reserve_task_reservation(
        &self,
        params: &TaskReservationReserveParams,
    ) -> Result<TaskReservationReserveResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            reserve_files_in_tx(tx, params)
        })
    }

    pub fn release_task_reservation(
        &self,
        params: &TaskReservationReleaseParams,
    ) -> Result<TaskReservationReleaseResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let mut expired_reservations = expire_reservations_in_scope(
                tx,
                &params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
                &now,
                TaskReservationScope::Files,
            )?;
            let existing = load_reservation_row(
                tx,
                &params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
                &params.reservation_id,
            )?;

            let Some(existing) = existing else {
                return Ok(TaskReservationReleaseResult {
                    released: false,
                    released_at: None,
                    reservation: None,
                    expired_reservations,
                });
            };

            if existing.released_at.is_some() {
                return Ok(TaskReservationReleaseResult {
                    released: false,
                    released_at: None,
                    reservation: None,
                    expired_reservations,
                });
            }

            let released_at = crate::now_string();
            let sql = format!(
                "UPDATE task_reservations
                 SET released_at = ?4,
                     release_reason = ?5,
                     release_metadata_json = ?6
                 WHERE {}
                   AND reservation_id = ?3
                   AND released_at IS NULL",
                reservation_scope_clause(TaskReservationScope::Files),
            );
            let affected = tx
                .tx
                .execute(
                    &sql,
                    params![
                        params.workspace_id.as_deref(),
                        params.workspace_orbit_dir,
                        params.reservation_id,
                        released_at,
                        params.release_reason.as_str(),
                        params.release_metadata_json.as_deref(),
                    ],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;

            if affected == 0 {
                expired_reservations.push(ExpiredTaskReservation {
                    reservation_id: params.reservation_id.clone(),
                    expired_at: now,
                });
                return Ok(TaskReservationReleaseResult {
                    released: false,
                    released_at: None,
                    reservation: None,
                    expired_reservations,
                });
            }

            Ok(TaskReservationReleaseResult {
                released: true,
                released_at: Some(released_at.clone()),
                reservation: Some(existing.into_released(
                    released_at,
                    params.release_reason,
                    params.release_metadata_json.clone(),
                )?),
                expired_reservations,
            })
        })
    }

    pub fn release_task_reservations_by_owner_run_id(
        &self,
        params: &TaskReservationReleaseByOwnerParams,
    ) -> Result<TaskReservationReleaseByOwnerResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let expired_reservations = expire_reservations_in_scope(
                tx,
                &params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
                &now,
                TaskReservationScope::Files,
            )?;
            let existing = load_active_reservations_by_owner(
                tx,
                &params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
                &params.owner_run_id,
            )?;
            if existing.is_empty() {
                return Ok(TaskReservationReleaseByOwnerResult {
                    released_reservations: Vec::new(),
                    expired_reservations,
                });
            }

            let released_at = crate::now_string();
            let sql = format!(
                "UPDATE task_reservations
                 SET released_at = ?4,
                     release_reason = ?5,
                     release_metadata_json = ?6
                 WHERE {}
                   AND owner_run_id = ?3
                   AND released_at IS NULL",
                reservation_scope_clause(TaskReservationScope::Files),
            );
            tx.tx
                .execute(
                    &sql,
                    params![
                        params.workspace_id.as_deref(),
                        params.workspace_orbit_dir,
                        params.owner_run_id,
                        released_at,
                        params.release_reason.as_str(),
                        params.release_metadata_json.as_deref(),
                    ],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;

            let released_reservations = existing
                .into_iter()
                .map(|row| {
                    row.into_released(
                        released_at.clone(),
                        params.release_reason,
                        params.release_metadata_json.clone(),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(TaskReservationReleaseByOwnerResult {
                released_reservations,
                expired_reservations,
            })
        })
    }

    pub fn list_owned_task_reservation_conflicts(
        &self,
        params: &TaskReservationOwnedConflictsParams,
    ) -> Result<TaskReservationOwnedConflictsResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let expired_reservations = expire_reservations_in_scope(
                tx,
                &params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
                &now,
                TaskReservationScope::Files,
            )?;
            let reservations = find_owned_reservation_conflicts(
                tx,
                &params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
                &now,
                &params.requested_files,
                params.limit,
            )?;
            Ok(TaskReservationOwnedConflictsResult {
                reservations,
                expired_reservations,
            })
        })
    }
}

/// Expire, conflict-check, and insert one file reservation inside a caller's
/// transaction.
///
/// [`Store::reserve_task_reservation`] is this function plus its own
/// transaction. The task/reservation commit boundary calls it *inside* the
/// transaction that also decides the commit, so a reservation and the task
/// transition it authorizes are one durable decision rather than two
/// independently committed writes.
pub(super) fn reserve_files_in_tx(
    tx: &mut crate::StoreTx<'_>,
    params: &TaskReservationReserveParams,
) -> Result<TaskReservationReserveResult, OrbitError> {
    let now = crate::now_string();
    let expired_reservations = expire_reservations_in_scope(
        tx,
        &params.workspace_orbit_dir,
        params.workspace_id.as_deref(),
        &now,
        TaskReservationScope::Files,
    )?;
    let conflicts = find_reservation_conflicts(
        tx,
        &params.workspace_orbit_dir,
        params.workspace_id.as_deref(),
        &now,
        &params.requested_files,
    )?;
    if !conflicts.is_empty() {
        return Ok(TaskReservationReserveResult {
            reserved: false,
            reservation_id: None,
            expires_at: None,
            reserved_files: Vec::new(),
            conflicts,
            expired_reservations,
        });
    }

    let reservation_id = unique_row_id("reservation-");
    let created_at = now;
    let expires_at = (Utc::now() + Duration::seconds(params.ttl_seconds as i64)).to_rfc3339();
    let task_ids_json = serialize_string_list(&params.task_ids)?;
    let files_json = serialize_string_list(&params.requested_files)?;

    tx.tx
        .execute(
            "INSERT INTO task_reservations(
                reservation_id,
                workspace_orbit_dir,
                workspace_id,
                task_ids_json,
                files_json,
                actor,
                created_at,
                expires_at,
                released_at,
                owner_run_id,
                owner_metadata_json,
                release_reason,
                release_metadata_json,
                scope
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, NULL, NULL, 'files')",
            params![
                reservation_id,
                params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
                task_ids_json,
                files_json,
                params.actor,
                created_at,
                expires_at,
                params.owner_run_id.as_deref(),
                params.owner_metadata_json.as_deref(),
            ],
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;

    Ok(TaskReservationReserveResult {
        reserved: true,
        reservation_id: Some(reservation_id),
        expires_at: Some(expires_at),
        reserved_files: params.requested_files.clone(),
        conflicts: Vec::new(),
        expired_reservations,
    })
}

/// Mark every TTL-expired row in `scope` released, returning what was expired.
///
/// [ORB-10709] Expiry is per-dimension so a file-reservation call never reports
/// an expired workspace claim as one of *its* expirations: the two dimensions
/// have separate audit vocabularies, and a caller emits events for whatever
/// this returns.
pub(super) fn expire_reservations_in_scope(
    tx: &mut crate::StoreTx<'_>,
    workspace_orbit_dir: &str,
    workspace_id: Option<&str>,
    now: &str,
    scope: TaskReservationScope,
) -> Result<Vec<ExpiredTaskReservation>, OrbitError> {
    let sql = format!(
        "SELECT reservation_id, expires_at
         FROM task_reservations
         WHERE {}
           AND released_at IS NULL
           AND expires_at <= ?3
         ORDER BY expires_at ASC, reservation_id ASC",
        reservation_scope_clause(scope),
    );
    let mut stmt = tx
        .tx
        .prepare(&sql)
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let rows = stmt
        .query_map(params![workspace_id, workspace_orbit_dir, now], |row| {
            Ok(ExpiredTaskReservation {
                reservation_id: row.get(0)?,
                expired_at: row.get(1)?,
            })
        })
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let expired_reservations = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| OrbitError::Store(error.to_string()))?;

    if !expired_reservations.is_empty() {
        let sql = format!(
            "UPDATE task_reservations
             SET released_at = ?3,
                 release_reason = ?4
             WHERE {}
               AND released_at IS NULL
               AND expires_at <= ?3",
            reservation_scope_clause(scope),
        );
        tx.tx
            .execute(
                &sql,
                params![
                    workspace_id,
                    workspace_orbit_dir,
                    now,
                    TaskReservationReleaseReason::TtlExpired.as_str(),
                ],
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
    }

    Ok(expired_reservations)
}

fn find_reservation_conflicts(
    tx: &mut crate::StoreTx<'_>,
    workspace_orbit_dir: &str,
    workspace_id: Option<&str>,
    now: &str,
    requested_files: &[String],
) -> Result<Vec<TaskLockConflict>, OrbitError> {
    let requested_files: BTreeSet<String> = requested_files.iter().cloned().collect();
    if requested_files.is_empty() {
        return Ok(Vec::new());
    }

    let sql = format!(
        "SELECT reservation_id, files_json
         FROM task_reservations
         WHERE {}
           AND released_at IS NULL
           AND expires_at > ?3
         ORDER BY created_at ASC, reservation_id ASC",
        reservation_scope_clause(TaskReservationScope::Files),
    );
    let mut stmt = tx
        .tx
        .prepare(&sql)
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let rows = stmt
        .query_map(params![workspace_id, workspace_orbit_dir, now], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| OrbitError::Store(error.to_string()))?;

    let mut conflicts = Vec::new();
    for row in rows {
        let (reservation_id, files_json) =
            row.map_err(|error| OrbitError::Store(error.to_string()))?;
        let reserved_files = parse_string_list(&files_json)?;
        for requested_file in &requested_files {
            if reserved_files
                .iter()
                .any(|held_file| workspace_relative_paths_overlap(requested_file, held_file))
            {
                conflicts.push(TaskLockConflict {
                    file: requested_file.clone(),
                    held_by: TaskLockHolder::Reservation,
                    held_by_id: reservation_id.clone(),
                });
            }
        }
    }

    conflicts.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.held_by_id.cmp(&right.held_by_id))
    });
    Ok(conflicts)
}

/// The workspace predicate, narrowed to one coordination dimension.
///
/// Parameter contract for every caller: ?1 is workspace_id, ?2 is
/// workspace_orbit_dir. V2 callers see workspace-bound rows plus legacy
/// NULL-workspace rows for the same orbit dir; legacy callers see all rows
/// scoped to that orbit dir.
///
/// [ORB-10709] `scope` is a required argument rather than a default so every
/// query site has to state which dimension it reads. File reservations and
/// workspace claims share this table and must never see each other's rows; a
/// forgotten filter would let a claim block the worker reservations it exists
/// to leave alone, so the compiler asks the question at each call site.
pub(super) fn reservation_scope_clause(scope: TaskReservationScope) -> String {
    // `as_str()` is a closed set of 'static literals, never caller input.
    format!(
        "(
        (?1 IS NOT NULL AND (workspace_id = ?1 OR (workspace_id IS NULL AND workspace_orbit_dir = ?2)))
        OR (?1 IS NULL AND workspace_orbit_dir = ?2)
    ) AND scope = '{}'",
        scope.as_str()
    )
}

#[derive(Debug, Clone)]
struct ReservationRow {
    reservation_id: String,
    workspace_id: Option<String>,
    task_ids_json: String,
    files_json: String,
    actor: String,
    created_at: String,
    expires_at: String,
    released_at: Option<String>,
    owner_run_id: Option<String>,
    owner_metadata_json: Option<String>,
}

impl ReservationRow {
    fn into_active(self) -> Result<ActiveTaskReservation, OrbitError> {
        Ok(ActiveTaskReservation {
            reservation_id: self.reservation_id,
            workspace_id: self.workspace_id,
            task_ids: parse_string_list(&self.task_ids_json)?,
            files: parse_string_list(&self.files_json)?,
            actor: self.actor,
            created_at: self.created_at,
            expires_at: self.expires_at,
            owner_run_id: self.owner_run_id,
            owner_metadata_json: self.owner_metadata_json,
        })
    }

    fn into_released(
        self,
        released_at: String,
        release_reason: TaskReservationReleaseReason,
        release_metadata_json: Option<String>,
    ) -> Result<ReleasedTaskReservation, OrbitError> {
        Ok(ReleasedTaskReservation {
            reservation_id: self.reservation_id,
            workspace_id: self.workspace_id,
            task_ids: parse_string_list(&self.task_ids_json)?,
            files: parse_string_list(&self.files_json)?,
            actor: self.actor,
            created_at: self.created_at,
            expires_at: self.expires_at,
            released_at,
            owner_run_id: self.owner_run_id,
            owner_metadata_json: self.owner_metadata_json,
            release_reason,
            release_metadata_json,
        })
    }
}

fn select_reservation_columns() -> &'static str {
    "reservation_id, workspace_id, task_ids_json, files_json, actor, created_at, expires_at,
     released_at, owner_run_id, owner_metadata_json"
}

fn reservation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReservationRow> {
    Ok(ReservationRow {
        reservation_id: row.get(0)?,
        workspace_id: row.get(1)?,
        task_ids_json: row.get(2)?,
        files_json: row.get(3)?,
        actor: row.get(4)?,
        created_at: row.get(5)?,
        expires_at: row.get(6)?,
        released_at: row.get(7)?,
        owner_run_id: row.get(8)?,
        owner_metadata_json: row.get(9)?,
    })
}

fn load_reservation_row(
    tx: &mut crate::StoreTx<'_>,
    workspace_orbit_dir: &str,
    workspace_id: Option<&str>,
    reservation_id: &str,
) -> Result<Option<ReservationRow>, OrbitError> {
    let sql = format!(
        "SELECT {}
         FROM task_reservations
         WHERE {}
           AND reservation_id = ?3",
        select_reservation_columns(),
        reservation_scope_clause(TaskReservationScope::Files),
    );
    tx.tx
        .query_row(
            &sql,
            params![workspace_id, workspace_orbit_dir, reservation_id],
            reservation_row,
        )
        .optional()
        .map_err(|error| OrbitError::Store(error.to_string()))
}

fn load_active_reservations_by_owner(
    tx: &mut crate::StoreTx<'_>,
    workspace_orbit_dir: &str,
    workspace_id: Option<&str>,
    owner_run_id: &str,
) -> Result<Vec<ReservationRow>, OrbitError> {
    let sql = format!(
        "SELECT {}
         FROM task_reservations
         WHERE {}
           AND owner_run_id = ?3
           AND released_at IS NULL
         ORDER BY created_at ASC, reservation_id ASC",
        select_reservation_columns(),
        reservation_scope_clause(TaskReservationScope::Files),
    );
    let mut stmt = tx
        .tx
        .prepare(&sql)
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let rows = stmt
        .query_map(
            params![workspace_id, workspace_orbit_dir, owner_run_id],
            reservation_row,
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| OrbitError::Store(error.to_string()))
}

fn find_owned_reservation_conflicts(
    tx: &mut crate::StoreTx<'_>,
    workspace_orbit_dir: &str,
    workspace_id: Option<&str>,
    now: &str,
    requested_files: &[String],
    limit: usize,
) -> Result<Vec<ActiveTaskReservation>, OrbitError> {
    let requested_files: BTreeSet<String> = requested_files.iter().cloned().collect();
    if requested_files.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let sql = format!(
        "SELECT {}
         FROM task_reservations
         WHERE {}
           AND released_at IS NULL
           AND expires_at > ?3
           AND owner_run_id IS NOT NULL
         ORDER BY created_at ASC, reservation_id ASC",
        select_reservation_columns(),
        reservation_scope_clause(TaskReservationScope::Files),
    );
    let mut stmt = tx
        .tx
        .prepare(&sql)
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let rows = stmt
        .query_map(
            params![workspace_id, workspace_orbit_dir, now],
            reservation_row,
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;

    let mut reservations = Vec::new();
    for row in rows {
        let row = row.map_err(|error| OrbitError::Store(error.to_string()))?;
        let reserved_files = parse_string_list(&row.files_json)?;
        let overlaps = requested_files.iter().any(|requested_file| {
            reserved_files
                .iter()
                .any(|held_file| workspace_relative_paths_overlap(requested_file, held_file))
        });
        if overlaps {
            reservations.push(row.into_active()?);
            if reservations.len() >= limit {
                break;
            }
        }
    }
    Ok(reservations)
}

fn parse_string_list(raw: &str) -> Result<Vec<String>, OrbitError> {
    serde_json::from_str(raw)
        .map_err(|error| OrbitError::Store(format!("deserialize reservation string list: {error}")))
}

fn serialize_string_list(values: &[String]) -> Result<String, OrbitError> {
    let unique = values.iter().cloned().collect::<BTreeSet<_>>();
    serde_json::to_string(&unique.into_iter().collect::<Vec<_>>())
        .map_err(|error| OrbitError::Store(format!("serialize reservation string list: {error}")))
}

#[cfg(test)]
#[cfg(test)]
mod tests;
