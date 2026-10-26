//! Path leases (`docs/design/path-leases`): `task_reservations` rows held by
//! a principal rather than a task.
//!
//! A lease row is a `files`-scope row with `kind = 'lease'`, empty task ids
//! and no owner run, so stale-reservation cleanup never classifies it and a
//! binary without leases reads it as an exclusive file reservation. A hard
//! lease blocks overlapping reservations and admission like a task
//! reservation; an advisory one is recorded and blocks nothing.

use chrono::{Duration, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::notices::{self, LeaseOverlapNotice};
use super::rows::{
    LEASE_ROWS, expire_leases, find_reservation_conflicts, parse_string_list, serialize_string_list,
};
use super::{reservation_scope_clause, unique_row_id};
use crate::Store;
use crate::contracts::{
    LeaseInsertParams, LeaseInsertResult, LeaseListResult, LeaseMode, LeasePrincipalClass,
    LeasePriority, LeaseRecord, LeaseReleaseParams, LeaseReleaseResult, LeaseRenewParams,
    LeaseRenewResult, TaskReservationReleaseReason, TaskReservationScope,
};
use serde_json::json;

const LEASE_ID_PREFIX: &str = "lease-";

impl Store {
    /// Record a lease. A hard lease is checked against active task
    /// reservations and hard leases in the inserting transaction, so two hard
    /// requests racing for one path cannot both be granted.
    pub fn insert_lease(
        &self,
        params: &LeaseInsertParams,
    ) -> Result<LeaseInsertResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let workspace_id = params.workspace_id.as_deref();
            let expired_leases =
                expire_leases(tx, &params.workspace_orbit_dir, workspace_id, &now)?;
            if params.mode == LeaseMode::Hard {
                let conflicts = find_reservation_conflicts(
                    tx,
                    &params.workspace_orbit_dir,
                    workspace_id,
                    &now,
                    &params.selectors,
                )?;
                if !conflicts.is_empty() {
                    return Ok(LeaseInsertResult {
                        lease: None,
                        conflicts,
                        expired_leases,
                    });
                }
            }

            let lease_id = unique_row_id(LEASE_ID_PREFIX);
            let expires_at = ttl_from_now(params.ttl_seconds);
            tx.tx
                .execute(
                    "INSERT INTO task_reservations(
                        reservation_id, workspace_orbit_dir, workspace_id, task_ids_json,
                        files_json, actor, created_at, expires_at, scope, kind, mode,
                        principal, principal_class, machine_id, priority, tags_json,
                        base_branch, bound_branch, heartbeat_at, token_id
                    ) VALUES (?1, ?2, ?3, '[]', ?4, ?5, ?6, ?7, ?8, 'lease', ?9, ?10, ?11,
                        ?12, ?13, ?14, ?15, ?16, ?6, ?17)",
                    params![
                        lease_id,
                        params.workspace_orbit_dir,
                        workspace_id,
                        serialize_string_list(&params.selectors)?,
                        params.actor,
                        now,
                        expires_at,
                        TaskReservationScope::Files.as_str(),
                        params.mode.as_str(),
                        params.principal,
                        params.principal_class.as_str(),
                        params.machine_id.as_deref(),
                        params.priority.as_str(),
                        serialize_string_list(&params.tags)?,
                        params.base_branch.as_deref(),
                        params.bound_branch.as_deref(),
                        params.token_id.as_deref(),
                    ],
                )
                .map_err(sqlite_store_error)?;
            notices::append_lease_notice(tx, &lease_id, "granted", &json!({
                "mode": params.mode, "principal": params.principal, "selectors": params.selectors,
            }), "granted")?;
            notices::append_lease_overlaps(tx, &LeaseOverlapNotice {
                orbit_dir: &params.workspace_orbit_dir, workspace_id,
                holder: &lease_id, paths: &params.selectors, kind: "overlap",
                payload: json!({"lease_id": lease_id, "mode": params.mode, "principal": params.principal}),
                event_key: &format!("lease:{lease_id}"), landing: None,
            })?;
            let lease = load_lease(tx, &params.workspace_orbit_dir, workspace_id, &lease_id)?;
            Ok(LeaseInsertResult {
                lease,
                conflicts: Vec::new(),
                expired_leases,
            })
        })
    }

    /// Move an active lease's `expires_at` forward and stamp its heartbeat.
    /// A released or expired lease is not revived.
    pub fn renew_lease(&self, params: &LeaseRenewParams) -> Result<LeaseRenewResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let workspace_id = params.workspace_id.as_deref();
            let expired_leases =
                expire_leases(tx, &params.workspace_orbit_dir, workspace_id, &now)?;
            let sql = format!(
                "UPDATE task_reservations
                 SET expires_at = ?4,
                     heartbeat_at = ?5
                 WHERE {}
                   AND {LEASE_ROWS}
                   AND reservation_id = ?3
                   AND released_at IS NULL
                   AND expires_at > ?5",
                reservation_scope_clause(TaskReservationScope::Files),
            );
            let renewed = tx
                .tx
                .execute(
                    &sql,
                    params![
                        workspace_id,
                        params.workspace_orbit_dir,
                        params.lease_id,
                        ttl_from_now(params.ttl_seconds),
                        now,
                    ],
                )
                .map_err(sqlite_store_error)?;
            let lease = if renewed == 0 {
                None
            } else {
                load_lease(
                    tx,
                    &params.workspace_orbit_dir,
                    workspace_id,
                    &params.lease_id,
                )?
            };
            Ok(LeaseRenewResult {
                lease,
                expired_leases,
            })
        })
    }

    /// End an active lease.
    pub fn release_lease(
        &self,
        params: &LeaseReleaseParams,
    ) -> Result<LeaseReleaseResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let workspace_id = params.workspace_id.as_deref();
            let expired_leases =
                expire_leases(tx, &params.workspace_orbit_dir, workspace_id, &now)?;
            let sql = format!(
                "UPDATE task_reservations
                 SET released_at = ?4,
                     release_reason = ?5,
                     release_metadata_json = ?6
                 WHERE {}
                   AND {LEASE_ROWS}
                   AND reservation_id = ?3
                   AND released_at IS NULL",
                reservation_scope_clause(TaskReservationScope::Files),
            );
            let released = tx
                .tx
                .execute(
                    &sql,
                    params![
                        workspace_id,
                        params.workspace_orbit_dir,
                        params.lease_id,
                        now,
                        params.release_reason.as_str(),
                        params.release_metadata_json.as_deref(),
                    ],
                )
                .map_err(sqlite_store_error)?;
            if released != 0 {
                notices::append_lease_notice(
                    tx,
                    &params.lease_id,
                    "released",
                    &json!({"reason": params.release_reason}),
                    "released",
                )?;
            }
            let lease = if released == 0 {
                None
            } else {
                load_lease(
                    tx,
                    &params.workspace_orbit_dir,
                    workspace_id,
                    &params.lease_id,
                )?
            };
            Ok(LeaseReleaseResult {
                lease,
                expired_leases,
            })
        })
    }

    /// Read one lease by id in any state, without expiring or otherwise
    /// mutating rows. `None` when it is unknown or belongs to another
    /// workspace.
    pub fn get_lease(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
        lease_id: &str,
    ) -> Result<Option<LeaseRecord>, OrbitError> {
        self.with_read_connection(|conn| {
            let sql = format!(
                "SELECT {LEASE_COLUMNS}
                 FROM task_reservations
                 WHERE {}
                   AND {LEASE_ROWS}
                   AND reservation_id = ?3",
                reservation_scope_clause(TaskReservationScope::Files),
            );
            conn.query_row(
                &sql,
                params![workspace_id, workspace_orbit_dir, lease_id],
                lease_row,
            )
            .optional()
            .map_err(sqlite_store_error)?
            .map(LeaseRow::into_record)
            .transpose()
        })
    }

    /// The workspace's active leases, advisory and hard, oldest first,
    /// without expiring or otherwise mutating rows: a lease past its TTL that
    /// lazy expiry has not yet ended is left out rather than ended.
    pub fn inspect_active_leases(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<Vec<LeaseRecord>, OrbitError> {
        self.with_read_connection(|conn| {
            let now = crate::now_string();
            let mut stmt = conn
                .prepare(&active_leases_sql())
                .map_err(sqlite_store_error)?;
            let rows = stmt
                .query_map(params![workspace_id, workspace_orbit_dir, now], lease_row)
                .map_err(sqlite_store_error)?;
            let mut leases = Vec::new();
            for row in rows {
                leases.push(row.map_err(sqlite_store_error)?.into_record()?);
            }
            Ok(leases)
        })
    }

    /// The workspace's active leases, oldest first, after expiring the ones
    /// whose TTL has passed.
    pub fn list_active_leases(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<LeaseListResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let expired_leases = expire_leases(tx, workspace_orbit_dir, workspace_id, &now)?;
            let mut stmt = tx
                .tx
                .prepare(&active_leases_sql())
                .map_err(sqlite_store_error)?;
            let rows = stmt
                .query_map(params![workspace_id, workspace_orbit_dir, now], lease_row)
                .map_err(sqlite_store_error)?;
            let mut leases = Vec::new();
            for row in rows {
                leases.push(row.map_err(sqlite_store_error)?.into_record()?);
            }
            Ok(LeaseListResult {
                leases,
                expired_leases,
            })
        })
    }

    /// Owner-clock maintenance. Expiry and warning writes are independent of
    /// the forge, and warnings are deduplicated for each grant/renewal period.
    pub fn sweep_lease_lifetimes(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<LeaseListResult, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = Utc::now();
            let now_text = now.to_rfc3339();
            let expired_leases = expire_leases(tx, workspace_orbit_dir, workspace_id, &now_text)?;
            let mut stmt = tx.tx.prepare(&active_leases_sql()).map_err(sqlite_store_error)?;
            let rows = stmt.query_map(params![workspace_id, workspace_orbit_dir, now_text], lease_row)
                .map_err(sqlite_store_error)?;
            let mut leases = Vec::new();
            for row in rows {
                leases.push(row.map_err(sqlite_store_error)?.into_record()?);
            }
            for lease in &leases {
                let heartbeat = chrono::DateTime::parse_from_rfc3339(&lease.heartbeat_at)
                    .map_err(|error| OrbitError::Store(format!("invalid lease heartbeat: {error}")))?;
                let expires = chrono::DateTime::parse_from_rfc3339(&lease.expires_at)
                    .map_err(|error| OrbitError::Store(format!("invalid lease expiry: {error}")))?;
                let warning_at = heartbeat + (expires - heartbeat) * 4 / 5;
                if now >= warning_at {
                    notices::append_lease_notice(tx, &lease.lease_id, "expiring",
                        &json!({"expires_at": lease.expires_at, "heartbeat_at": lease.heartbeat_at}),
                        &format!("expiring:{}:{}", lease.heartbeat_at, lease.expires_at))?;
                }
            }
            Ok(LeaseListResult { leases, expired_leases })
        })
    }
}

/// Active leases, oldest first; binds ?1 and ?2 as
/// [`reservation_scope_clause`] does and ?3 as now.
fn active_leases_sql() -> String {
    format!(
        "SELECT {LEASE_COLUMNS}
         FROM task_reservations
         WHERE {}
           AND {LEASE_ROWS}
           AND released_at IS NULL
           AND expires_at > ?3
         ORDER BY created_at ASC, reservation_id ASC",
        reservation_scope_clause(TaskReservationScope::Files),
    )
}

const LEASE_COLUMNS: &str = "reservation_id, workspace_id, files_json, mode, priority, tags_json,
     principal, principal_class, machine_id, base_branch, bound_branch, actor, created_at,
     expires_at, heartbeat_at, released_at, release_reason, notice_cursor, token_id";

struct LeaseRow {
    lease_id: String,
    workspace_id: Option<String>,
    files_json: String,
    mode: String,
    priority: Option<String>,
    tags_json: Option<String>,
    principal: Option<String>,
    principal_class: Option<String>,
    machine_id: Option<String>,
    base_branch: Option<String>,
    bound_branch: Option<String>,
    actor: String,
    created_at: String,
    expires_at: String,
    heartbeat_at: Option<String>,
    released_at: Option<String>,
    release_reason: Option<String>,
    notice_cursor: u64,
    token_id: Option<String>,
}

fn lease_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LeaseRow> {
    Ok(LeaseRow {
        lease_id: row.get(0)?,
        workspace_id: row.get(1)?,
        files_json: row.get(2)?,
        mode: row.get(3)?,
        priority: row.get(4)?,
        tags_json: row.get(5)?,
        principal: row.get(6)?,
        principal_class: row.get(7)?,
        machine_id: row.get(8)?,
        base_branch: row.get(9)?,
        bound_branch: row.get(10)?,
        actor: row.get(11)?,
        created_at: row.get(12)?,
        expires_at: row.get(13)?,
        heartbeat_at: row.get(14)?,
        released_at: row.get(15)?,
        release_reason: row.get(16)?,
        notice_cursor: row.get(17)?,
        token_id: row.get(18)?,
    })
}

impl LeaseRow {
    fn into_record(self) -> Result<LeaseRecord, OrbitError> {
        let invalid =
            |message: String| OrbitError::Store(format!("lease {}: {message}", self.lease_id));
        let required = |field: &str, value: &Option<String>| {
            value
                .clone()
                .ok_or_else(|| invalid(format!("missing {field}")))
        };
        let principal = required("principal", &self.principal)?;
        let principal_class =
            LeasePrincipalClass::parse(&required("principal class", &self.principal_class)?)
                .map_err(invalid)?;
        let priority =
            LeasePriority::parse(&required("priority", &self.priority)?).map_err(invalid)?;
        let tags = match self.tags_json.as_deref() {
            Some(raw) => parse_string_list(raw)?,
            None => Vec::new(),
        };
        let release_reason = match self.release_reason.as_deref() {
            Some(raw) => Some(
                serde_json::from_value::<TaskReservationReleaseReason>(serde_json::Value::from(
                    raw,
                ))
                .map_err(|error| invalid(format!("release reason '{raw}': {error}")))?,
            ),
            None => None,
        };
        Ok(LeaseRecord {
            notice_cursor: self.notice_cursor,
            selectors: parse_string_list(&self.files_json)?,
            mode: LeaseMode::parse(&self.mode),
            priority,
            tags,
            principal,
            principal_class,
            machine_id: self.machine_id,
            base_branch: self.base_branch,
            bound_branch: self.bound_branch,
            heartbeat_at: self.heartbeat_at.unwrap_or_else(|| self.created_at.clone()),
            lease_id: self.lease_id,
            workspace_id: self.workspace_id,
            actor: self.actor,
            created_at: self.created_at,
            expires_at: self.expires_at,
            released_at: self.released_at,
            release_reason,
            token_id: self.token_id,
        })
    }
}

fn load_lease(
    tx: &mut crate::StoreTx<'_>,
    workspace_orbit_dir: &str,
    workspace_id: Option<&str>,
    lease_id: &str,
) -> Result<Option<LeaseRecord>, OrbitError> {
    let sql = format!(
        "SELECT {LEASE_COLUMNS}
         FROM task_reservations
         WHERE {}
           AND {LEASE_ROWS}
           AND reservation_id = ?3",
        reservation_scope_clause(TaskReservationScope::Files),
    );
    tx.tx
        .query_row(
            &sql,
            params![workspace_id, workspace_orbit_dir, lease_id],
            lease_row,
        )
        .optional()
        .map_err(sqlite_store_error)?
        .map(LeaseRow::into_record)
        .transpose()
}

fn ttl_from_now(ttl_seconds: u32) -> String {
    (Utc::now() + Duration::seconds(i64::from(ttl_seconds))).to_rfc3339()
}
