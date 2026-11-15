//! Immutable dispatch provenance, separate from optional completion telemetry.
use crate::contracts::{AgentExecutionBackfill, AgentExecutionQuery, AgentExecutionRecord};
use crate::{Store, now_string};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::telemetry::AgentExecutionIdentity;

impl Store {
    /// Persist before starting a provider so running and interrupted steps keep
    /// the same provenance as completed ones. The returned id links telemetry
    /// exactly, without matching by time, model, or mutable crew configuration.
    pub fn begin_agent_execution(
        &self,
        workspace: &str,
        run_id: &str,
        activity_id: &str,
        provider: &str,
        model: Option<&str>,
        identity: &AgentExecutionIdentity,
    ) -> Result<i64, OrbitError> {
        let json = serde_json::to_string(identity)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let conn = orbit_common::fs::lock_wait::mutex(&self.conn)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        conn.execute("INSERT INTO agent_executions(workspace_id, run_id, activity_id, provider, model, identity_json, started_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![workspace, run_id, activity_id, provider, model, json, now_string()])
            .map_err(sqlite_store_error)?;
        Ok(conn.last_insert_rowid())
    }

    /// Read dispatch identities for one workspace-qualified run, in launch order.
    pub fn agent_executions(
        &self,
        workspace: &str,
        run_id: &str,
    ) -> Result<Vec<AgentExecutionRecord>, OrbitError> {
        let conn = self.read()?;
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
        rows.map(|row| {
            let (id, run_id, activity_id, provider, model, json, at) =
                row.map_err(sqlite_store_error)?;
            let stored: StoredAgentExecutionIdentity = serde_json::from_str(&json)
                .map_err(|error| OrbitError::Store(error.to_string()))?;
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
        })
        .collect()
    }

    /// Read launch identities in a workspace and date window, in launch
    /// order. Window bounds compare the stored RFC 3339 `started_at` text,
    /// as the invocation accounting read compares its timestamps.
    pub fn list_agent_executions(
        &self,
        query: &AgentExecutionQuery,
    ) -> Result<Vec<AgentExecutionRecord>, OrbitError> {
        let conn = self.read()?;
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
        rows.map(|row| {
            let (id, run_id, activity_id, provider, model, json, at) =
                row.map_err(sqlite_store_error)?;
            let stored: StoredAgentExecutionIdentity = serde_json::from_str(&json)
                .map_err(|error| OrbitError::Store(error.to_string()))?;
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
        })
        .collect()
    }
}

/// `identity_json` as stored: the dispatch identity, plus the provenance of a
/// backfilled one. Readers that want only the identity deserialize
/// [`AgentExecutionIdentity`] from the same text and ignore `backfill`.
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct StoredAgentExecutionIdentity {
    #[serde(flatten)]
    pub(super) identity: AgentExecutionIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) backfill: Option<AgentExecutionBackfill>,
}
