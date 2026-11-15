//! Row decoding for run and step reads: column mapping, JSON columns and enum parsing.

use std::collections::HashMap;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::{JobRun, JobRunState, JobRunStep, JobTargetType};

use super::super::input_keys::parse_input_keys;
use super::STEP_RUN_ID_CHUNK;
use super::list_sql::JOB_RUN_COLUMNS;
use crate::parse_timestamp;

pub(in crate::driver::sqlite::job_run_store) fn get_job_run_for_workspace_conn(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    run_id: &str,
) -> Result<Option<JobRun>, OrbitError> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {JOB_RUN_COLUMNS} FROM job_runs WHERE workspace_id = ?1 AND run_id = ?2"
        ))
        .map_err(sqlite_store_error)?;
    let mut run = match stmt.query_row(rusqlite::params![workspace_id, run_id], row_to_job_run) {
        Ok(run) => run,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(err) => return Err(sqlite_store_error(err)),
    };
    run.steps = read_steps(conn, workspace_id, run_id)?;
    Ok(Some(run))
}

pub(in crate::driver::sqlite::job_run_store) fn row_to_job_run(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<JobRun> {
    row_to_job_run_with_input_keys(row, None)
}

/// [`row_to_job_run`], hydrating only `input_keys` of the run's input when
/// set ([`JobRunQuery::input_keys`]).
pub(super) fn row_to_job_run_with_input_keys(
    row: &rusqlite::Row<'_>,
    input_keys: Option<&[&str]>,
) -> rusqlite::Result<JobRun> {
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
        build: parse_optional_json(row.get(17)?, "build_json")?,
        executed_on: parse_optional_json(row.get(16)?, "executed_on_json")?,
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
        input: match input_keys {
            Some(keys) => parse_optional_json_with(input_json, "input_json", |raw| {
                parse_input_keys(raw, keys)
            })?,
            None => parse_optional_json(input_json, "input_json")?,
        },
        retry_source_run_id: row.get(12)?,
        knowledge_metrics: parse_optional_json(knowledge_metrics_json, "knowledge_metrics_json")?,
        resolved_crew: row.get(14)?,
        crew_model: row.get(15)?,
        steps: Vec::new(),
    })
}

/// Steps for a page of runs in one query per chunk instead of one per run.
pub(in crate::driver::sqlite::job_run_store) fn read_steps_for_runs(
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
                 agent_response_json, lock_wait_ms FROM job_run_steps \
                 WHERE workspace_id = ?1 AND run_id IN ({placeholders}) \
                 ORDER BY run_id ASC, step_index ASC"
            ))
            .map_err(sqlite_store_error)?;
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
            .map_err(sqlite_store_error)?;
        for row in rows {
            let (run_id, step) = row.map_err(sqlite_store_error)?;
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
             duration_ms, exit_code, error_code, error_message, agent_response_json, lock_wait_ms \
             FROM job_run_steps WHERE workspace_id = ?1 AND run_id = ?2 ORDER BY step_index ASC",
        )
        .map_err(sqlite_store_error)?;
    let rows = stmt
        .query_map(rusqlite::params![workspace_id, run_id], row_to_job_run_step)
        .map_err(sqlite_store_error)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
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
    let error_code: Option<String> = row.get(offset + 8)?;
    Ok(JobRunStep {
        lock_wait_ms: row
            .get::<_, Option<i64>>(offset + 11)?
            .map(|value| value as u64),
        step_index: step_index as u32,
        target_type: parse_job_target_type(&target_type_raw)?,
        target_id: row.get(offset + 2)?,
        state: parse_job_run_state(&state_raw)?,
        started_at: parse_optional_timestamp(started_raw)?,
        finished_at: parse_optional_timestamp(finished_raw)?,
        duration_ms: duration_ms.map(|value| value as u64),
        exit_code: row.get(offset + 7)?,
        // The `error_code` column is the typed code's storage; a record
        // whose column is empty has no typed code and classifies through
        // the legacy marker fallback.
        failure_code: error_code
            .as_deref()
            .map(orbit_types::workflow::StepFailureCode::parse),
        error_code,
        error_message: row.get(offset + 9)?,
        agent_response_json: parse_optional_json(agent_response_json, "agent_response_json")?,
    })
}

pub(super) fn optional_json<T: serde::Serialize>(
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
    parse_optional_json_with(raw, label, |raw| serde_json::from_str(raw))
}

fn parse_optional_json_with<T>(
    raw: Option<String>,
    label: &str,
    parse: impl FnOnce(&str) -> serde_json::Result<T>,
) -> rusqlite::Result<Option<T>> {
    raw.map(|raw| {
        parse(&raw).map_err(|e| {
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

pub(super) fn parse_job_run_state(raw: &str) -> rusqlite::Result<JobRunState> {
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
