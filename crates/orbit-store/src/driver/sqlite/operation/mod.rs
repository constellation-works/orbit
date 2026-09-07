//! Operation-mode grants and recovery ledgers in the host SQLite database
//! [ORB-11332].
//!
//! Grants are read inside the child-admission transaction
//! (`job_run_store::admit_child_job_run`), so stop, expiry, and revocation
//! share SQLite's process-wide writer lock with child creation: whichever
//! commits first defines the order every process sees.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::{
    GrantStatus, GrantTransition, JobRunState, OperationGrant, RecoveryEpisode, RecoveryLedger,
    RecoveryReservation,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::str::FromStr;

use crate::Store;
use crate::contracts::{
    ChildAdmissionAuthority, GrantInsertOutcome, GrantTransitionKind, GrantTransitionOutcome,
    GrantTransitionRequest, OperationStoreBackend, RecoveryReserveRequest,
};
use crate::driver::sqlite::migration::FeatureMigration;

/// Bound on the grant history one listing returns.
const MAX_GRANT_LISTING: usize = 100;

pub(crate) fn initialize(store: &Store) -> Result<(), OrbitError> {
    store.apply_feature_migrations(
        "operation",
        &[FeatureMigration::new(1, "grants_and_recovery_ledgers", |conn| {
            conn.execute_batch(
                "CREATE TABLE operation_grants (workspace_id TEXT NOT NULL, grant_id TEXT NOT NULL, status TEXT NOT NULL, revision INTEGER NOT NULL, created_at TEXT NOT NULL, expires_at TEXT NOT NULL, grant_json TEXT NOT NULL, PRIMARY KEY(workspace_id, grant_id));
                 CREATE INDEX operation_grants_workspace ON operation_grants(workspace_id, created_at);
                 CREATE TABLE operation_recovery (workspace_id TEXT NOT NULL, task_id TEXT NOT NULL, ledger_json TEXT NOT NULL, PRIMARY KEY(workspace_id, task_id));",
            )
            .map_err(|error| OrbitError::Store(error.to_string()))
        })],
    )
}

fn encode<T: serde::Serialize>(value: &T) -> Result<String, OrbitError> {
    serde_json::to_string(value).map_err(|error| OrbitError::Store(error.to_string()))
}

fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, OrbitError> {
    serde_json::from_str(raw)
        .map_err(|error| OrbitError::Store(format!("invalid persisted operation record: {error}")))
}

fn read_grant(
    conn: &Connection,
    workspace_id: &str,
    grant_id: &str,
) -> Result<Option<OperationGrant>, OrbitError> {
    conn.query_row(
        "SELECT grant_json FROM operation_grants WHERE workspace_id=?1 AND grant_id=?2",
        params![workspace_id, grant_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|error| OrbitError::Store(error.to_string()))?
    .map(|raw| decode(&raw))
    .transpose()
}

fn write_grant(conn: &Connection, grant: &OperationGrant) -> Result<(), OrbitError> {
    conn.execute(
        "UPDATE operation_grants SET status=?3, revision=?4, expires_at=?5, grant_json=?6 \
         WHERE workspace_id=?1 AND grant_id=?2",
        params![
            grant.workspace_id,
            grant.id,
            grant.status.as_str(),
            grant.revision,
            grant.expires_at.to_rfc3339(),
            encode(grant)?
        ],
    )
    .map_err(|error| OrbitError::Store(error.to_string()))?;
    Ok(())
}

fn active_grant_id(
    conn: &Connection,
    workspace_id: &str,
    now: DateTime<Utc>,
) -> Result<Option<String>, OrbitError> {
    conn.query_row(
        "SELECT grant_id FROM operation_grants WHERE workspace_id=?1 AND status='active' AND expires_at > ?2 \
         ORDER BY created_at DESC LIMIT 1",
        params![workspace_id, now.to_rfc3339()],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// Task ids one leaf run carries, from its persisted input.
fn run_task_ids(input_json: Option<&str>) -> Vec<String> {
    let Some(input) =
        input_json.and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
    else {
        return Vec::new();
    };
    let mut ids = input
        .get("task_ids")
        .and_then(serde_json::Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(id) = input.get("task_id").and_then(serde_json::Value::as_str) {
        ids.push(id.to_string());
    }
    ids
}

/// The reason a grant-bound child admission must be refused, or `None` when
/// the grant, scope, task claim, and capacity all admit it. Runs inside the
/// caller's admission transaction.
pub(crate) fn admission_refusal(
    conn: &Connection,
    workspace_id: &str,
    job_id: &str,
    authority: &ChildAdmissionAuthority,
) -> Result<Option<String>, OrbitError> {
    let Some(grant) = read_grant(conn, workspace_id, &authority.grant_id)? else {
        return Ok(Some("grant_missing".to_string()));
    };
    // The live grant's own answer names what happened (stop, expiry,
    // revocation) before the coarser revision check, so a stale coordinator
    // learns the reason rather than only that something moved.
    let admission = grant.admission(authority.now);
    if !admission.admits() {
        return Ok(Some(admission.reason().to_string()));
    }
    if grant.revision != authority.grant_revision {
        return Ok(Some("grant_revision_changed".to_string()));
    }
    if let Some(task_id) = &authority.task_id
        && !grant.covers(task_id)
    {
        return Ok(Some("outside_grant_scope".to_string()));
    }
    if authority.task_id.is_none() && authority.leaf_ceiling.is_none() {
        return Ok(None);
    }

    let mut statement = conn
        .prepare("SELECT state, input_json FROM job_runs WHERE workspace_id=?1 AND job_id=?2")
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let rows = statement
        .query_map(params![workspace_id, job_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let mut live = 0_u32;
    for row in rows {
        let (state, input_json) = row.map_err(|error| OrbitError::Store(error.to_string()))?;
        let state = JobRunState::from_str(&state)
            .map_err(|error| OrbitError::Store(format!("invalid job run state: {error}")))?;
        if state.is_terminal() {
            continue;
        }
        live += 1;
        if let Some(task_id) = &authority.task_id
            && run_task_ids(input_json.as_deref())
                .iter()
                .any(|carried| carried == task_id)
        {
            return Ok(Some("task_claimed".to_string()));
        }
    }
    if authority
        .leaf_ceiling
        .is_some_and(|ceiling| live >= ceiling)
    {
        return Ok(Some("capacity_saturated".to_string()));
    }
    Ok(None)
}

fn read_ledger(
    conn: &Connection,
    workspace_id: &str,
    task_id: &str,
) -> Result<Option<RecoveryLedger>, OrbitError> {
    conn.query_row(
        "SELECT ledger_json FROM operation_recovery WHERE workspace_id=?1 AND task_id=?2",
        params![workspace_id, task_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|error| OrbitError::Store(error.to_string()))?
    .map(|raw| decode(&raw))
    .transpose()
}

fn write_ledger(
    conn: &Connection,
    workspace_id: &str,
    ledger: &RecoveryLedger,
) -> Result<(), OrbitError> {
    conn.execute(
        "INSERT INTO operation_recovery (workspace_id, task_id, ledger_json) VALUES (?1,?2,?3) \
         ON CONFLICT(workspace_id, task_id) DO UPDATE SET ledger_json=excluded.ledger_json",
        params![workspace_id, ledger.task_id, encode(ledger)?],
    )
    .map_err(|error| OrbitError::Store(error.to_string()))?;
    Ok(())
}

impl OperationStoreBackend for Store {
    fn operation_grant_insert(
        &self,
        grant: &OperationGrant,
    ) -> Result<GrantInsertOutcome, OrbitError> {
        initialize(self)?;
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            if let Some(existing) = active_grant_id(conn, &grant.workspace_id, grant.created_at)? {
                return Ok(GrantInsertOutcome::ActiveGrantExists(existing));
            }
            conn.execute(
                "INSERT INTO operation_grants VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    grant.workspace_id,
                    grant.id,
                    grant.status.as_str(),
                    grant.revision,
                    grant.created_at.to_rfc3339(),
                    grant.expires_at.to_rfc3339(),
                    encode(grant)?
                ],
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(GrantInsertOutcome::Inserted)
        })
    }

    fn operation_grant(
        &self,
        workspace_id: &str,
        grant_id: &str,
    ) -> Result<Option<OperationGrant>, OrbitError> {
        initialize(self)?;
        self.with_read_connection(|conn| read_grant(conn, workspace_id, grant_id))
    }

    fn operation_active_grant(
        &self,
        workspace_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<OperationGrant>, OrbitError> {
        initialize(self)?;
        self.with_read_connection(|conn| {
            let Some(grant_id) = active_grant_id(conn, workspace_id, now)? else {
                return Ok(None);
            };
            read_grant(conn, workspace_id, &grant_id)
        })
    }

    fn operation_grants(
        &self,
        workspace_id: &str,
        limit: usize,
    ) -> Result<Vec<OperationGrant>, OrbitError> {
        initialize(self)?;
        self.with_read_connection(|conn| {
            let mut statement = conn
                .prepare(
                    "SELECT grant_json FROM operation_grants WHERE workspace_id=?1 \
                     ORDER BY created_at DESC, grant_id DESC LIMIT ?2",
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            let rows = statement
                .query_map(params![workspace_id, limit.min(MAX_GRANT_LISTING)], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            rows.map(|row| {
                row.map_err(|error| OrbitError::Store(error.to_string()))
                    .and_then(|raw| decode(&raw))
            })
            .collect()
        })
    }

    fn operation_grant_transition(
        &self,
        workspace_id: &str,
        grant_id: &str,
        request: &GrantTransitionRequest<'_>,
    ) -> Result<GrantTransitionOutcome, OrbitError> {
        initialize(self)?;
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let Some(mut grant) = read_grant(conn, workspace_id, grant_id)? else {
                return Ok(GrantTransitionOutcome::NotFound);
            };
            if request
                .expected_revision
                .is_some_and(|expected| expected != grant.revision)
            {
                return Ok(GrantTransitionOutcome::RevisionConflict(grant));
            }

            let transition = GrantTransition {
                actor: request.actor.to_string(),
                reason: request.reason.map(str::to_string),
                at: request.now,
            };
            match request.kind {
                GrantTransitionKind::Stop => {
                    if grant.status != GrantStatus::Active {
                        return Ok(GrantTransitionOutcome::Unchanged(grant));
                    }
                    grant.status = GrantStatus::Stopped;
                    grant.stopped = Some(transition);
                }
                GrantTransitionKind::Revoke => {
                    if grant.status == GrantStatus::Revoked {
                        return Ok(GrantTransitionOutcome::Unchanged(grant));
                    }
                    grant.status = GrantStatus::Revoked;
                    grant.revoked = Some(transition);
                }
            }
            grant.revision = grant.revision.saturating_add(1);
            write_grant(conn, &grant)?;
            Ok(GrantTransitionOutcome::Applied(grant))
        })
    }

    fn operation_recovery_reserve(
        &self,
        workspace_id: &str,
        request: &RecoveryReserveRequest<'_>,
    ) -> Result<(RecoveryReservation, RecoveryLedger), OrbitError> {
        initialize(self)?;
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let mut ledger = read_ledger(conn, workspace_id, request.task_id)?
                .unwrap_or_else(|| RecoveryLedger::new(request.task_id, request.now));
            let consumed = ledger.episodes_consumed();
            if consumed >= request.budget.episodes {
                return Ok((
                    RecoveryReservation::Exhausted {
                        reason: "recovery_episodes_exhausted",
                        episodes_consumed: consumed,
                        consumed_seconds: ledger.consumed_seconds,
                    },
                    ledger,
                ));
            }
            if ledger.consumed_seconds >= request.budget.seconds {
                return Ok((
                    RecoveryReservation::Exhausted {
                        reason: "recovery_minutes_exhausted",
                        episodes_consumed: consumed,
                        consumed_seconds: ledger.consumed_seconds,
                    },
                    ledger,
                ));
            }

            let episode = consumed.saturating_add(1);
            ledger.episodes.push(RecoveryEpisode {
                index: episode,
                kind: request.kind,
                run_id: request.run_id.to_string(),
                step_id: request.step_id.map(str::to_string),
                reserved_at: request.now,
                elapsed_seconds: None,
            });
            ledger.updated_at = request.now;
            write_ledger(conn, workspace_id, &ledger)?;
            Ok((
                RecoveryReservation::Reserved {
                    episode,
                    remaining_episodes: request.budget.episodes.saturating_sub(episode),
                    remaining_seconds: request
                        .budget
                        .seconds
                        .saturating_sub(ledger.consumed_seconds),
                },
                ledger,
            ))
        })
    }

    fn operation_recovery_settle(
        &self,
        workspace_id: &str,
        task_id: &str,
        episode: u32,
        elapsed_seconds: u64,
        now: DateTime<Utc>,
    ) -> Result<RecoveryLedger, OrbitError> {
        initialize(self)?;
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn = tx.connection();
            let mut ledger = read_ledger(conn, workspace_id, task_id)?.ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "no recovery ledger for task '{task_id}' to settle"
                ))
            })?;
            let entry = ledger
                .episodes
                .iter_mut()
                .find(|entry| entry.index == episode)
                .ok_or_else(|| {
                    OrbitError::InvalidInput(format!(
                        "recovery episode {episode} of task '{task_id}' was never reserved"
                    ))
                })?;
            // Settling twice records the larger observation once, never twice.
            let previously = entry.elapsed_seconds.unwrap_or(0);
            let elapsed = elapsed_seconds.max(previously);
            entry.elapsed_seconds = Some(elapsed);
            ledger.consumed_seconds = ledger
                .consumed_seconds
                .saturating_sub(previously)
                .saturating_add(elapsed);
            ledger.updated_at = now;
            write_ledger(conn, workspace_id, &ledger)?;
            Ok(ledger)
        })
    }

    fn operation_recovery_ledger(
        &self,
        workspace_id: &str,
        task_id: &str,
    ) -> Result<Option<RecoveryLedger>, OrbitError> {
        initialize(self)?;
        self.with_read_connection(|conn| read_ledger(conn, workspace_id, task_id))
    }
}

#[cfg(test)]
mod tests;
