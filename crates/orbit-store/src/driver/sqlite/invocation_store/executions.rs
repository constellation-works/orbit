//! Immutable dispatch provenance, separate from optional completion telemetry.
use crate::contracts::AgentExecutionRecord;
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
            Ok(AgentExecutionRecord {
                id,
                run_id,
                activity_id,
                provider,
                model,
                identity: serde_json::from_str(&json)
                    .map_err(|error| OrbitError::Store(error.to_string()))?,
                started_at: crate::parse_timestamp(&at).map_err(sqlite_store_error)?,
            })
        })
        .collect()
    }
}
