//! Reads and writes for recording a dispatch identity after the fact, for
//! invocations that completed before identities were recorded at dispatch.
//! Nothing here runs when the store opens; an operator command drives it.
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::telemetry::AgentExecutionIdentity;

use super::executions::StoredAgentExecutionIdentity;
use crate::Store;
use crate::contracts::{
    AgentExecutionBackfill, InvocationLaunchEvent, UnidentifiedInvocationLaunch,
};

/// The audit event that records each provider launch and its rendered argv.
pub(crate) const CLI_INVOCATION_STARTED_EVENT: &str = "cli.invocation.started";

impl Store {
    /// Every completed invocation of `workspace_id` without a dispatch
    /// identity, grouped by run and oldest first within it, each with the launch events that may have
    /// started it: same workspace, run and model, launched within
    /// `tolerance` of the invocation's end minus its duration.
    pub fn list_unidentified_invocation_launches(
        &self,
        workspace_id: &str,
        tolerance: chrono::Duration,
    ) -> Result<Vec<UnidentifiedInvocationLaunch>, OrbitError> {
        let conn = self.read()?;
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
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)?;
        drop(stmt);

        let mut events_stmt = conn
            .prepare(
                "SELECT event_id, ts, payload_json FROM v2_audit_events \
                 WHERE workspace_id = ?1 AND run_id = ?2 AND event_type = ?3 ORDER BY id",
            )
            .map_err(sqlite_store_error)?;
        // Rows arrive grouped by run, so only one run's launches (whose argv
        // can be long) are held at a time.
        let mut run_launches: (Option<String>, Vec<RecordedLaunch>) = (None, Vec::new());
        let mut unidentified = Vec::with_capacity(rows.len());
        for (invocation_id, run_id, activity_id, model, ended_at, duration_ms, crew) in rows {
            if run_launches.0.as_deref() != Some(run_id.as_str()) {
                let launches = events_stmt
                    .query_map(
                        rusqlite::params![workspace_id, run_id, CLI_INVOCATION_STARTED_EVENT],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .map_err(sqlite_store_error)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(sqlite_store_error)?
                    .into_iter()
                    .filter_map(|(event_id, ts, payload)| {
                        RecordedLaunch::parse(event_id, &ts, &payload)
                    })
                    .collect();
                run_launches = (Some(run_id.clone()), launches);
            }
            // A row whose end time does not parse cannot be placed in time,
            // so it gets no candidates and stays unknown.
            let launched_at = crate::parse_timestamp(&ended_at)
                .ok()
                .map(|ended| ended - chrono::Duration::milliseconds(duration_ms.max(0)));
            let launches = match launched_at {
                Some(launched_at) => run_launches
                    .1
                    .iter()
                    .filter(|launch| {
                        launch.model == model
                            && (launch.event.started_at - launched_at).abs() <= tolerance
                    })
                    .map(|launch| launch.event.clone())
                    .collect(),
                None => Vec::new(),
            };
            unidentified.push(UnidentifiedInvocationLaunch {
                invocation_id,
                run_id,
                activity_id,
                run_resolved_crew: crew.filter(|crew| !crew.trim().is_empty()),
                launches,
            });
        }
        Ok(unidentified)
    }

    /// Record `identity` for one invocation that still has no dispatch
    /// identity, with its backfill provenance, and link the invocation to it.
    /// Returns false, writing nothing, when the invocation is missing or
    /// already has an identity, so running a backfill again changes nothing.
    pub fn record_backfilled_agent_execution(
        &self,
        workspace_id: &str,
        invocation_id: i64,
        launched_at: DateTime<Utc>,
        identity: &AgentExecutionIdentity,
        backfill: &AgentExecutionBackfill,
    ) -> Result<bool, OrbitError> {
        let json = serde_json::to_string(&StoredAgentExecutionIdentity {
            identity: identity.clone(),
            backfill: Some(backfill.clone()),
        })
        .map_err(|error| OrbitError::Store(error.to_string()))?;
        let mut conn = orbit_common::fs::lock_wait::mutex(&self.conn)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let tx = conn.transaction().map_err(sqlite_store_error)?;
        let invocation = {
            let mut stmt = tx
                .prepare(
                    "SELECT job_run_id, activity_id, COALESCE(provider, agent), model \
                     FROM invocations \
                     WHERE id = ?1 AND workspace_id = ?2 AND agent_execution_id IS NULL",
                )
                .map_err(sqlite_store_error)?;
            let mut rows = stmt
                .query_map(rusqlite::params![invocation_id, workspace_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                })
                .map_err(sqlite_store_error)?;
            rows.next().transpose().map_err(sqlite_store_error)?
        };
        let Some((run_id, activity_id, provider, model)) = invocation else {
            return Ok(false);
        };
        tx.execute(
            "INSERT INTO agent_executions(workspace_id, run_id, activity_id, provider, model, identity_json, started_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                workspace_id,
                run_id,
                activity_id,
                provider,
                model,
                json,
                launched_at.to_rfc3339()
            ],
        )
        .map_err(sqlite_store_error)?;
        let execution_id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE invocations SET agent_execution_id = ?1 WHERE id = ?2 AND agent_execution_id IS NULL",
            rusqlite::params![execution_id, invocation_id],
        )
        .map_err(sqlite_store_error)?;
        tx.commit().map_err(sqlite_store_error)?;
        Ok(true)
    }
}

/// A launch event with the model it launched, before matching.
struct RecordedLaunch {
    model: Option<String>,
    event: InvocationLaunchEvent,
}

impl RecordedLaunch {
    /// A payload that is not a launch record with an argv is no evidence.
    fn parse(event_id: String, ts: &str, payload: &str) -> Option<Self> {
        let started_at = crate::parse_timestamp(ts).ok()?;
        let payload: serde_json::Value = serde_json::from_str(payload).ok()?;
        let argv_redacted = payload
            .get("argv_redacted")?
            .as_array()?
            .iter()
            .map(|arg| arg.as_str().map(str::to_string))
            .collect::<Option<Vec<_>>>()?;
        let model = payload
            .get("model")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        Some(Self {
            model,
            event: InvocationLaunchEvent {
                event_id,
                started_at,
                argv_redacted,
            },
        })
    }
}
