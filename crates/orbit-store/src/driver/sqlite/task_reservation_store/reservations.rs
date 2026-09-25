//! The `Store` reservation API: inspect, list, check, reserve, release and
//! owned-conflict queries.

use super::rows::{
    find_owned_reservation_conflicts, find_reservation_conflicts,
    load_active_reservations_by_owner, load_reservation_row, reservation_row, reserve_files_in_tx,
    select_reservation_columns,
};
use super::{expire_reservations_in_scope, reservation_scope_clause};
use crate::{
    ActiveTaskReservation, ExpiredTaskReservation, Store, TaskReservationCheckParams,
    TaskReservationCheckResult, TaskReservationListResult, TaskReservationOwnedConflictsParams,
    TaskReservationOwnedConflictsResult, TaskReservationReleaseByOwnerParams,
    TaskReservationReleaseByOwnerResult, TaskReservationReleaseParams,
    TaskReservationReleaseResult, TaskReservationReserveParams, TaskReservationReserveResult,
    TaskReservationScope,
};
use orbit_common::OrbitError;
use rusqlite::{TransactionBehavior, params};

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
