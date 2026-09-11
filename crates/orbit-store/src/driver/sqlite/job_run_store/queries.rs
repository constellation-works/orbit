//! Run and step SQL, row mapping, and shared filter helpers on [`Store`].

use std::collections::HashMap;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::{
    JobRun, JobRunState, JobRunStep, JobTargetType, PipelineState, RunIdRole, run_id_candidate,
    run_id_minute_stem,
};
use rusqlite::OptionalExtension;

use crate::contracts::{JobRunOrder, JobRunQuery};
use crate::{Store, parse_timestamp};

/// Run ids per `IN (...)` list, under SQLite's bound-parameter cap.
pub(super) const STEP_RUN_ID_CHUNK: usize = 500;

impl Store {
    pub fn upsert_job_run_for_workspace(
        &self,
        workspace_id: &str,
        run: &JobRun,
        pipeline_state: Option<&PipelineState>,
    ) -> Result<(), OrbitError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        upsert_job_run_for_workspace_conn(&conn, workspace_id, run, pipeline_state)
    }

    pub fn upsert_job_run_step_for_workspace(
        &self,
        workspace_id: &str,
        run_id: &str,
        step: &JobRunStep,
    ) -> Result<(), OrbitError> {
        let agent_response_json = optional_json(&step.agent_response_json, "agent response")?;
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        conn.execute(
            r#"INSERT INTO job_run_steps(
                workspace_id, run_id, step_index, target_type, target_id, state,
                started_at, finished_at, duration_ms, exit_code, error_code,
                error_message, agent_response_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
            ON CONFLICT(workspace_id, run_id, step_index) DO UPDATE SET
                target_type = excluded.target_type,
                target_id = excluded.target_id,
                state = excluded.state,
                started_at = excluded.started_at,
                finished_at = excluded.finished_at,
                duration_ms = excluded.duration_ms,
                exit_code = excluded.exit_code,
                error_code = excluded.error_code,
                error_message = excluded.error_message,
                agent_response_json = excluded.agent_response_json"#,
            rusqlite::params![
                workspace_id,
                run_id,
                i64::from(step.step_index),
                step.target_type.to_string(),
                step.target_id,
                step.state.to_string(),
                step.started_at.map(|ts| ts.to_rfc3339()),
                step.finished_at.map(|ts| ts.to_rfc3339()),
                step.duration_ms.map(|value| value as i64),
                step.exit_code,
                step.error_code,
                step.error_message,
                agent_response_json,
            ],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(())
    }

    pub fn get_job_run_for_workspace(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Option<JobRun>, OrbitError> {
        let conn = self.read()?;
        get_job_run_for_workspace_conn(&conn, workspace_id, run_id)
    }

    pub fn list_job_runs_for_workspace(
        &self,
        workspace_id: &str,
        query: &JobRunQuery,
    ) -> Result<Vec<JobRun>, OrbitError> {
        let (where_clause, mut params) = job_run_filter_sql(workspace_id, query);
        let order_clause = job_run_order_sql(query.order_by);
        let mut sql = format!(
            "SELECT run_id, job_id, attempt, state, scheduled_at, started_at, finished_at, \
             duration_ms, created_at, pid, pid_start_time, input_json, retry_source_run_id, \
             knowledge_metrics_json, resolved_crew, COALESCE(crew_model, implementer_model) \
             FROM job_runs WHERE {where_clause} ORDER BY {order_clause}"
        );
        if let Some(limit) = query.limit {
            sql.push_str(&format!(" LIMIT ?{}", params.len() + 1));
            params.push(Box::new(limit as i64));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|b| b.as_ref()).collect();
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map(param_refs.as_slice(), row_to_job_run)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let mut runs = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        drop(stmt);
        let run_ids = runs
            .iter()
            .map(|run| run.run_id.clone())
            .collect::<Vec<_>>();
        let mut steps_by_run = read_steps_for_runs(&conn, workspace_id, &run_ids)?;
        for run in &mut runs {
            run.steps = steps_by_run.remove(&run.run_id).unwrap_or_default();
        }
        Ok(runs)
    }

    /// `COUNT(*)` over the same filter `list_job_runs_for_workspace` applies,
    /// ignoring `limit`: a tile that only needs a number must not hydrate
    /// (and silently cap at) a page of rows to get it.
    pub fn count_job_runs_for_workspace(
        &self,
        workspace_id: &str,
        query: &JobRunQuery,
    ) -> Result<u64, OrbitError> {
        let (where_clause, params) = job_run_filter_sql(workspace_id, query);
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|b| b.as_ref()).collect();
        let conn = self.read()?;
        conn.query_row(
            &format!("SELECT COUNT(*) FROM job_runs WHERE {where_clause}"),
            param_refs.as_slice(),
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count.max(0) as u64)
        .map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Every recorded `duration_ms` matching the filter, ignoring `limit`.
    /// Feeds percentile baselines without materializing whole runs.
    pub fn list_job_run_durations_for_workspace(
        &self,
        workspace_id: &str,
        query: &JobRunQuery,
    ) -> Result<Vec<u64>, OrbitError> {
        let (where_clause, params) = job_run_filter_sql(workspace_id, query);
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|b| b.as_ref()).collect();
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT duration_ms FROM job_runs \
                 WHERE {where_clause} AND duration_ms IS NOT NULL"
            ))
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| row.get::<_, i64>(0))
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        rows.map(|row| row.map(|value| value.max(0) as u64))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    pub fn delete_job_run_for_workspace(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<bool, OrbitError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        conn.execute(
            "DELETE FROM job_runs WHERE workspace_id = ?1 AND run_id = ?2",
            rusqlite::params![workspace_id, run_id],
        )
        .map(|count| count > 0)
        .map_err(|e| OrbitError::Store(e.to_string()))
    }
}

pub(super) fn upsert_job_run_for_workspace_conn(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    run: &JobRun,
    pipeline_state: Option<&PipelineState>,
) -> Result<(), OrbitError> {
    let input_json = optional_json(&run.input, "job run input")?;
    let knowledge_metrics_json =
        optional_json(&run.knowledge_metrics, "job run knowledge metrics")?;
    let pipeline_state_json = optional_json(&pipeline_state, "job run pipeline state")?;
    conn.execute(
        r#"INSERT INTO job_runs(
            run_id, workspace_id, job_id, attempt, state, scheduled_at,
            started_at, finished_at, duration_ms, created_at, pid, pid_start_time,
            input_json, retry_source_run_id, knowledge_metrics_json, resolved_crew,
            crew_model, pipeline_state_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
        ON CONFLICT(workspace_id, run_id) DO UPDATE SET
            job_id = excluded.job_id,
            attempt = excluded.attempt,
            state = excluded.state,
            scheduled_at = excluded.scheduled_at,
            started_at = excluded.started_at,
            finished_at = excluded.finished_at,
            duration_ms = excluded.duration_ms,
            created_at = excluded.created_at,
            pid = excluded.pid,
            pid_start_time = excluded.pid_start_time,
            input_json = excluded.input_json,
            retry_source_run_id = excluded.retry_source_run_id,
            knowledge_metrics_json = excluded.knowledge_metrics_json,
            resolved_crew = excluded.resolved_crew,
            crew_model = excluded.crew_model,
            pipeline_state_json = COALESCE(excluded.pipeline_state_json, job_runs.pipeline_state_json)"#,
        rusqlite::params![
            run.run_id,
            workspace_id,
            run.job_id,
            i64::from(run.attempt),
            run.state.to_string(),
            run.scheduled_at.to_rfc3339(),
            run.started_at.map(|ts| ts.to_rfc3339()),
            run.finished_at.map(|ts| ts.to_rfc3339()),
            run.duration_ms.map(|value| value as i64),
            run.created_at.to_rfc3339(),
            run.pid.map(i64::from),
            run.pid_start_time,
            input_json,
            run.retry_source_run_id,
            knowledge_metrics_json,
            run.resolved_crew,
            run.crew_model,
            pipeline_state_json,
        ],
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;
    Ok(())
}

/// Sequence ceiling for one role inside one minute stem.
const MAX_RUN_ID_SEQUENCE: u32 = 1023;

/// Allocate the next free run id of `role` for the minute `submitted_at` falls
/// in.
///
/// Each role numbers its own sequence, so a run's children never consume the
/// numbers its top-level siblings would take and neither borrows the other's
/// shape [ORB-12111]. Call this inside the same transaction that inserts the
/// run: the probe below is only as good as the write it commits with.
///
/// Exhausting the sequence is an error rather than a fallback id. Roughly a
/// thousand runs of one role in one workspace inside one minute is already
/// pathological, and any id returned without a free-slot probe behind it would
/// upsert over the live run already holding it.
pub(super) fn next_run_id_conn(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    role: RunIdRole,
    submitted_at: DateTime<Utc>,
) -> Result<String, OrbitError> {
    let stem = run_id_minute_stem(submitted_at);
    for sequence in 1..=MAX_RUN_ID_SEQUENCE {
        let candidate = run_id_candidate(&stem, role, sequence);
        let exists = conn
            .query_row(
                "SELECT 1 FROM job_runs WHERE workspace_id = ?1 AND run_id = ?2",
                rusqlite::params![workspace_id, candidate],
                |_| Ok(()),
            )
            .optional()
            .map_err(|error| OrbitError::Store(error.to_string()))?
            .is_some();
        if !exists {
            return Ok(candidate);
        }
    }

    Err(OrbitError::Store(format!(
        "run id sequence exhausted: {MAX_RUN_ID_SEQUENCE} {role} runs already recorded for {stem}"
    )))
}

pub(super) fn get_job_run_for_workspace_conn(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    run_id: &str,
) -> Result<Option<JobRun>, OrbitError> {
    let mut stmt = conn
        .prepare(
            "SELECT run_id, job_id, attempt, state, scheduled_at, started_at, finished_at, \
             duration_ms, created_at, pid, pid_start_time, input_json, retry_source_run_id, \
             knowledge_metrics_json, resolved_crew, COALESCE(crew_model, implementer_model) \
             FROM job_runs WHERE workspace_id = ?1 AND run_id = ?2",
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let mut run = match stmt.query_row(rusqlite::params![workspace_id, run_id], row_to_job_run) {
        Ok(run) => run,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(err) => return Err(OrbitError::Store(err.to_string())),
    };
    run.steps = read_steps(conn, workspace_id, run_id)?;
    Ok(Some(run))
}

fn row_to_job_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRun> {
    let attempt: i64 = row.get(2)?;
    let state_raw: String = row.get(3)?;
    let scheduled_raw: String = row.get(4)?;
    let started_raw: Option<String> = row.get(5)?;
    let finished_raw: Option<String> = row.get(6)?;
    let duration_ms: Option<i64> = row.get(7)?;
    let created_raw: String = row.get(8)?;
    let pid: Option<i64> = row.get(9)?;
    let input_json: Option<String> = row.get(11)?;
    let knowledge_metrics_json: Option<String> = row.get(13)?;
    Ok(JobRun {
        run_id: row.get(0)?,
        job_id: row.get(1)?,
        attempt: attempt as u32,
        state: parse_job_run_state(&state_raw)?,
        scheduled_at: parse_timestamp(&scheduled_raw)?,
        started_at: parse_optional_timestamp(started_raw)?,
        finished_at: parse_optional_timestamp(finished_raw)?,
        duration_ms: duration_ms.map(|value| value as u64),
        created_at: parse_timestamp(&created_raw)?,
        pid: pid.map(|value| value as u32),
        pid_start_time: row.get(10)?,
        input: parse_optional_json(input_json, "input_json")?,
        retry_source_run_id: row.get(12)?,
        knowledge_metrics: parse_optional_json(knowledge_metrics_json, "knowledge_metrics_json")?,
        resolved_crew: row.get(14)?,
        crew_model: row.get(15)?,
        steps: Vec::new(),
    })
}

/// `WHERE` clause and bound parameters for a [`JobRunQuery`] on `job_runs`,
/// shared by the list, count, and duration reads so the three cannot drift.
fn job_run_filter_sql(
    workspace_id: &str,
    query: &JobRunQuery,
) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut conditions = vec!["workspace_id = ?1".to_string()];
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(workspace_id.to_string())];
    if let Some(job_id) = &query.job_id {
        conditions.push(format!("job_id = ?{}", params.len() + 1));
        params.push(Box::new(job_id.clone()));
    }
    if let Some(state) = query.state {
        conditions.push(format!("state = ?{}", params.len() + 1));
        params.push(Box::new(state.to_string()));
    }
    if query.terminal_only {
        conditions.push(
            "state IN ('success', 'failed', 'timeout', 'cancelled', 'interrupted')".to_string(),
        );
    }
    if query.active_only {
        conditions.push("state IN ('pending', 'running')".to_string());
    }
    if let Some(created_since) = query.created_since {
        conditions.push(format!("created_at >= ?{}", params.len() + 1));
        params.push(Box::new(created_since.to_rfc3339()));
    }
    (conditions.join(" AND "), params)
}

/// `ORDER BY` clause for a bounded [`JobRunQuery`], matched to
/// [`JobRunOrder`]. `run_id ASC` breaks ties deterministically in both
/// variants; timestamps are stored as fixed-width RFC 3339 text, so a
/// lexical `DESC` sort matches chronological order. `CreatedAt` is covered by
/// `idx_job_runs_workspace_created`; `Recency` is a query-time expression
/// over the same rows the `workspace_id` prefix of that index already
/// narrows to, so a per-workspace sort stays cheap without a dedicated index
/// [ORB-11251].
fn job_run_order_sql(order_by: JobRunOrder) -> &'static str {
    match order_by {
        JobRunOrder::CreatedAt => "created_at DESC, run_id ASC",
        JobRunOrder::Recency => "COALESCE(finished_at, started_at, created_at) DESC, run_id ASC",
    }
}

/// Steps for a page of runs in one query per chunk instead of one per run.
fn read_steps_for_runs(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    run_ids: &[String],
) -> Result<HashMap<String, Vec<JobRunStep>>, OrbitError> {
    let mut grouped: HashMap<String, Vec<JobRunStep>> = HashMap::new();
    for chunk in run_ids.chunks(STEP_RUN_ID_CHUNK) {
        let placeholders = (0..chunk.len())
            .map(|index| format!("?{}", index + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = conn
            .prepare(&format!(
                "SELECT run_id, step_index, target_type, target_id, state, started_at, \
                 finished_at, duration_ms, exit_code, error_code, error_message, \
                 agent_response_json FROM job_run_steps \
                 WHERE workspace_id = ?1 AND run_id IN ({placeholders}) \
                 ORDER BY run_id ASC, step_index ASC"
            ))
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> =
            vec![Box::new(workspace_id.to_string())];
        params.extend(
            chunk
                .iter()
                .map(|run_id| Box::new(run_id.clone()) as Box<dyn rusqlite::types::ToSql>),
        );
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|b| b.as_ref()).collect();
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                let run_id: String = row.get(0)?;
                Ok((run_id, row_to_job_run_step_at(row, 1)?))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        for row in rows {
            let (run_id, step) = row.map_err(|e| OrbitError::Store(e.to_string()))?;
            grouped.entry(run_id).or_default().push(step);
        }
    }
    Ok(grouped)
}

fn read_steps(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    run_id: &str,
) -> Result<Vec<JobRunStep>, OrbitError> {
    let mut stmt = conn
        .prepare(
            "SELECT step_index, target_type, target_id, state, started_at, finished_at, \
             duration_ms, exit_code, error_code, error_message, agent_response_json \
             FROM job_run_steps WHERE workspace_id = ?1 AND run_id = ?2 ORDER BY step_index ASC",
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let rows = stmt
        .query_map(rusqlite::params![workspace_id, run_id], row_to_job_run_step)
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| OrbitError::Store(e.to_string()))
}

fn row_to_job_run_step(row: &rusqlite::Row<'_>) -> rusqlite::Result<JobRunStep> {
    row_to_job_run_step_at(row, 0)
}

/// Decode a step whose columns start at `offset` (0 for the per-run read,
/// 1 when a leading `run_id` column is selected alongside).
fn row_to_job_run_step_at(row: &rusqlite::Row<'_>, offset: usize) -> rusqlite::Result<JobRunStep> {
    let step_index: i64 = row.get(offset)?;
    let target_type_raw: String = row.get(offset + 1)?;
    let state_raw: String = row.get(offset + 3)?;
    let started_raw: Option<String> = row.get(offset + 4)?;
    let finished_raw: Option<String> = row.get(offset + 5)?;
    let duration_ms: Option<i64> = row.get(offset + 6)?;
    let agent_response_json: Option<String> = row.get(offset + 10)?;
    Ok(JobRunStep {
        step_index: step_index as u32,
        target_type: parse_job_target_type(&target_type_raw)?,
        target_id: row.get(offset + 2)?,
        state: parse_job_run_state(&state_raw)?,
        started_at: parse_optional_timestamp(started_raw)?,
        finished_at: parse_optional_timestamp(finished_raw)?,
        duration_ms: duration_ms.map(|value| value as u64),
        exit_code: row.get(offset + 7)?,
        error_code: row.get(offset + 8)?,
        error_message: row.get(offset + 9)?,
        agent_response_json: parse_optional_json(agent_response_json, "agent_response_json")?,
    })
}

fn optional_json<T: serde::Serialize>(
    value: &Option<T>,
    label: &str,
) -> Result<Option<String>, OrbitError> {
    value
        .as_ref()
        .map(|value| {
            serde_json::to_string(value)
                .map_err(|e| OrbitError::Store(format!("serialize {label}: {e}")))
        })
        .transpose()
}

fn parse_optional_json<T: serde::de::DeserializeOwned>(
    raw: Option<String>,
    label: &str,
) -> rusqlite::Result<Option<T>> {
    raw.map(|raw| {
        serde_json::from_str(&raw).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                raw.len(),
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid {label}: {e}"),
                )),
            )
        })
    })
    .transpose()
}

fn parse_optional_timestamp(raw: Option<String>) -> rusqlite::Result<Option<DateTime<Utc>>> {
    raw.map(|raw| parse_timestamp(&raw)).transpose()
}

fn parse_job_run_state(raw: &str) -> rusqlite::Result<JobRunState> {
    JobRunState::from_str(raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            raw.len(),
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })
}

fn parse_job_target_type(raw: &str) -> rusqlite::Result<JobTargetType> {
    JobTargetType::from_str(raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            raw.len(),
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        )
    })
}
