//! Immutable dispatch provenance (`agent_executions`), separate from optional
//! completion telemetry.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::telemetry::AgentExecutionIdentity;
use rusqlite::{Connection, OptionalExtension};

use crate::contract::{AgentExecutionBackfill, AgentExecutionQuery, AgentExecutionRecord};
use crate::now_string;

/// One completed invocation without an `agent_execution_id`, for the backfill workflow.
pub(crate) struct UnidentifiedInvocationRow {
    pub(crate) id: i64,
    pub(crate) run_id: String,
    pub(crate) activity_id: String,
    pub(crate) model: Option<String>,
    pub(crate) ended_at: String,
    pub(crate) duration_ms: i64,
    pub(crate) run_resolved_crew: Option<String>,
}

/// Dispatch slot read from an unidentified invocation row inside a write transaction.
pub(crate) struct UnidentifiedInvocationTarget {
    pub(crate) run_id: String,
    pub(crate) activity_id: String,
    pub(crate) provider: String,
    pub(crate) model: Option<String>,
}

/// Decoded existing `agent_executions` row for coverage checks in backfill.
pub(crate) struct AgentExecutionSlotRow {
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) backfill: Option<AgentExecutionBackfill>,
}

/// `identity_json` as stored: the dispatch identity, plus the provenance of a
/// backfilled one. Readers that want only the identity deserialize
/// [`AgentExecutionIdentity`] from the same text and ignore `backfill`.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredAgentExecutionIdentity {
    #[serde(flatten)]
    identity: AgentExecutionIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    backfill: Option<AgentExecutionBackfill>,
}

pub(super) fn insert_agent_execution(
    conn: &Connection,
    workspace: &str,
    run_id: &str,
    activity_id: &str,
    provider: &str,
    model: Option<&str>,
    identity: &AgentExecutionIdentity,
) -> Result<i64, OrbitError> {
    let json =
        serde_json::to_string(identity).map_err(|error| OrbitError::Store(error.to_string()))?;
    conn.execute(
        "INSERT INTO agent_executions(workspace_id, run_id, activity_id, provider, model, identity_json, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![workspace, run_id, activity_id, provider, model, json, now_string()],
    )
    .map_err(sqlite_store_error)?;
    Ok(conn.last_insert_rowid())
}

/// The dispatch slot and launch time of one backfilled agent execution.
pub(crate) struct BackfilledLaunch<'a> {
    pub(crate) workspace: &'a str,
    pub(crate) run_id: &'a str,
    pub(crate) activity_id: &'a str,
    pub(crate) provider: &'a str,
    pub(crate) model: Option<&'a str>,
    pub(crate) started_at: DateTime<Utc>,
}

pub(super) fn insert_agent_execution_with_backfill(
    conn: &Connection,
    launch: &BackfilledLaunch<'_>,
    identity: &AgentExecutionIdentity,
    backfill: &AgentExecutionBackfill,
) -> Result<i64, OrbitError> {
    let BackfilledLaunch {
        workspace,
        run_id,
        activity_id,
        provider,
        model,
        started_at,
    } = *launch;
    let json = serde_json::to_string(&StoredAgentExecutionIdentity {
        identity: identity.clone(),
        backfill: Some(backfill.clone()),
    })
    .map_err(|error| OrbitError::Store(error.to_string()))?;
    conn.execute(
        "INSERT INTO agent_executions(workspace_id, run_id, activity_id, provider, model, identity_json, started_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            workspace,
            run_id,
            activity_id,
            provider,
            model,
            json,
            started_at.to_rfc3339()
        ],
    )
    .map_err(sqlite_store_error)?;
    Ok(conn.last_insert_rowid())
}

pub(super) fn link_agent_execution(
    conn: &Connection,
    invocation_id: i64,
    execution_id: i64,
) -> Result<(), OrbitError> {
    conn.execute(
        "UPDATE invocations SET agent_execution_id = ?1 WHERE id = ?2 AND agent_execution_id IS NULL",
        rusqlite::params![execution_id, invocation_id],
    )
    .map_err(sqlite_store_error)?;
    Ok(())
}

pub(super) fn agent_executions(
    conn: &Connection,
    workspace: &str,
    run_id: &str,
) -> Result<Vec<AgentExecutionRecord>, OrbitError> {
    let mut stmt = conn
        .prepare(
            "SELECT id, run_id, activity_id, provider, model, identity_json, started_at
         FROM agent_executions WHERE workspace_id = ?1 AND run_id = ?2 ORDER BY id",
        )
        .map_err(sqlite_store_error)?;
    let rows = stmt
        .query_map(rusqlite::params![workspace, run_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(sqlite_store_error)?;
    rows.map(decode_execution_row).collect()
}

pub(super) fn list_agent_executions(
    conn: &Connection,
    query: &AgentExecutionQuery,
) -> Result<Vec<AgentExecutionRecord>, OrbitError> {
    let mut sql = String::from(
        "SELECT id, run_id, activity_id, provider, model, identity_json, started_at \
         FROM agent_executions WHERE started_at < ?",
    );
    let mut params: Vec<String> = vec![query.until.to_rfc3339()];
    if let Some(workspace_id) = &query.workspace_id {
        sql.push_str(" AND workspace_id = ?");
        params.push(workspace_id.clone());
    }
    if let Some(since) = query.since {
        sql.push_str(" AND started_at >= ?");
        params.push(since.to_rfc3339());
    }
    sql.push_str(" ORDER BY id");
    let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(sqlite_store_error)?;
    rows.map(decode_execution_row).collect()
}

pub(super) fn list_unidentified(
    conn: &Connection,
    workspace_id: &str,
) -> Result<Vec<UnidentifiedInvocationRow>, OrbitError> {
    let mut stmt = conn
        .prepare(
            "SELECT i.id, i.job_run_id, i.activity_id, i.model, i.ts, i.duration_ms, \
                    (SELECT r.resolved_crew FROM job_runs r \
                      WHERE r.workspace_id = i.workspace_id AND r.run_id = i.job_run_id \
                      LIMIT 1) \
             FROM invocations i \
             WHERE i.workspace_id = ?1 AND i.agent_execution_id IS NULL \
             ORDER BY i.job_run_id, i.id",
        )
        .map_err(sqlite_store_error)?;
    let rows = stmt
        .query_map(rusqlite::params![workspace_id], |row| {
            Ok(UnidentifiedInvocationRow {
                id: row.get(0)?,
                run_id: row.get(1)?,
                activity_id: row.get(2)?,
                model: row.get(3)?,
                ended_at: row.get(4)?,
                duration_ms: row.get(5)?,
                run_resolved_crew: row.get(6)?,
            })
        })
        .map_err(sqlite_store_error)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
}

pub(super) fn unidentified_target(
    conn: &Connection,
    workspace_id: &str,
    invocation_id: i64,
) -> Result<Option<UnidentifiedInvocationTarget>, OrbitError> {
    conn.query_row(
        "SELECT job_run_id, activity_id, COALESCE(provider, agent), model \
         FROM invocations \
         WHERE id = ?1 AND workspace_id = ?2 AND agent_execution_id IS NULL",
        rusqlite::params![invocation_id, workspace_id],
        |row| {
            Ok(UnidentifiedInvocationTarget {
                run_id: row.get(0)?,
                activity_id: row.get(1)?,
                provider: row.get(2)?,
                model: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(sqlite_store_error)
}

pub(super) fn list_job_run_crews(
    conn: &Connection,
    workspace_id: &str,
) -> Result<HashMap<String, Option<String>>, OrbitError> {
    let mut stmt = conn
        .prepare("SELECT run_id, resolved_crew FROM job_runs WHERE workspace_id = ?1")
        .map_err(sqlite_store_error)?;
    let rows = stmt
        .query_map(rusqlite::params![workspace_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(sqlite_store_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_store_error)
}

pub(super) fn agent_executions_for_slot(
    conn: &Connection,
    workspace_id: &str,
    run_id: &str,
    activity_id: &str,
    provider: &str,
    model: Option<&str>,
) -> Result<Vec<AgentExecutionSlotRow>, OrbitError> {
    let mut stmt = conn
        .prepare(
            "SELECT identity_json, started_at FROM agent_executions \
             WHERE workspace_id = ?1 AND run_id = ?2 AND activity_id = ?3 \
               AND provider = ?4 AND model IS ?5",
        )
        .map_err(sqlite_store_error)?;
    let rows = stmt
        .query_map(
            rusqlite::params![workspace_id, run_id, activity_id, provider, model],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .map_err(sqlite_store_error)?;
    rows.map(|row| {
        let (identity_json, started_at) = row.map_err(sqlite_store_error)?;
        let stored: StoredAgentExecutionIdentity = serde_json::from_str(&identity_json)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let started_at = crate::parse_timestamp(&started_at).map_err(sqlite_store_error)?;
        Ok(AgentExecutionSlotRow {
            started_at,
            backfill: stored.backfill,
        })
    })
    .collect()
}

type RawExecutionRow = (i64, String, String, String, Option<String>, String, String);

fn decode_execution_row(
    row: rusqlite::Result<RawExecutionRow>,
) -> Result<AgentExecutionRecord, OrbitError> {
    let (id, run_id, activity_id, provider, model, json, at) = row.map_err(sqlite_store_error)?;
    let stored: StoredAgentExecutionIdentity =
        serde_json::from_str(&json).map_err(|error| OrbitError::Store(error.to_string()))?;
    Ok(AgentExecutionRecord {
        id,
        run_id,
        activity_id,
        provider,
        model,
        identity: stored.identity,
        started_at: crate::parse_timestamp(&at).map_err(sqlite_store_error)?,
        backfill: stored.backfill,
    })
}
