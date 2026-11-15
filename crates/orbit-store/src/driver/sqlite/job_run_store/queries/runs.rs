//! Public run and step methods on `Store`: listing, counting, upserts and deletes.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::{JobRun, JobRunState, JobRunStep, PipelineState};
use rusqlite::OptionalExtension;

use super::list_sql::{JOB_RUN_COLUMNS, job_run_filter_sql, job_run_list_sql, latest_job_runs_sql};
use super::rows::{
    get_job_run_for_workspace_conn, optional_json, parse_job_run_state, read_steps_for_runs,
    row_to_job_run, row_to_job_run_with_input_keys,
};
use super::write::upsert_job_run_for_workspace_conn;
use super::{LATEST_JOB_ID_CHUNK, STEP_RUN_ID_CHUNK};
use crate::Store;
use crate::contracts::{JOB_RUN_TASK_INPUT_KEYS, JobRunQuery};

impl Store {
    /// Project only the ownership bindings of selected runs, without decoding
    /// run rows or reading checkpoints/steps. Selection has no age window
    /// unless the caller supplies one in `query`.
    pub fn job_run_task_ids_for_workspace(
        &self,
        workspace_id: &str,
        query: &JobRunQuery,
    ) -> Result<std::collections::BTreeSet<String>, OrbitError> {
        let (selected, params) = if query.limit.is_some() {
            job_run_list_sql(workspace_id, query)
        } else {
            // Without a run limit the ID set needs no recency sort or other
            // run columns. Retain exactly the listing surface's predicates.
            let (filter, params) = job_run_filter_sql(workspace_id, query);
            (
                format!("SELECT run_id FROM job_runs WHERE {filter}"),
                params,
            )
        };
        // `job_run_tasks` (v59) materializes exactly the text `task_ids` /
        // `task_id` bindings this projection returns, trigger-maintained on
        // every write and backfilled, so read it joined to the selected
        // runs instead of walking every selected run's `input_json`. The
        // selected runs still decide membership (same predicates, same
        // limit/order surface); a malformed binding in one run simply has
        // no row there and cannot refuse the others.
        let sql = format!(
            "WITH selected_runs AS ({selected}) \
             SELECT job_run_tasks.task_id FROM job_run_tasks \
             JOIN selected_runs ON selected_runs.run_id = job_run_tasks.run_id \
             WHERE job_run_tasks.workspace_id = ?1"
        );
        let conn = self.read()?;
        let mut statement = conn.prepare(&sql).map_err(sqlite_store_error)?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                row.get::<_, String>(0)
            })
            .map_err(sqlite_store_error)?;
        let mut ids = std::collections::BTreeSet::new();
        for row in rows {
            let id = row.map_err(sqlite_store_error)?;
            let id = id.trim();
            if !id.is_empty() {
                ids.insert(id.to_string());
            }
        }
        Ok(ids)
    }

    pub(in crate::driver::sqlite::job_run_store) fn latest_job_runs_for_workspace(
        &self,
        workspace_id: &str,
        job_ids: &[String],
    ) -> Result<Vec<JobRun>, OrbitError> {
        if job_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut ids = job_ids.iter().collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        let conn = self.read()?;
        let mut runs = Vec::new();
        for chunk in ids.chunks(LATEST_JOB_ID_CHUNK) {
            let sql = latest_job_runs_sql(chunk.len());
            let mut params: Vec<&dyn rusqlite::types::ToSql> = vec![&workspace_id];
            params.extend(chunk.iter().map(|id| *id as &dyn rusqlite::types::ToSql));
            let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;
            let rows = stmt
                .query_map(params.as_slice(), row_to_job_run)
                .map_err(sqlite_store_error)?;
            runs.extend(
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(sqlite_store_error)?,
            );
        }
        let run_ids = runs
            .iter()
            .map(|run| run.run_id.clone())
            .collect::<Vec<_>>();
        let mut steps = read_steps_for_runs(&conn, workspace_id, &run_ids)?;
        for run in &mut runs {
            run.steps = steps.remove(&run.run_id).unwrap_or_default();
        }
        runs.sort_by(|a, b| a.job_id.cmp(&b.job_id));
        Ok(runs)
    }

    pub fn upsert_job_run_for_workspace(
        &self,
        workspace_id: &str,
        run: &JobRun,
        pipeline_state: Option<&PipelineState>,
    ) -> Result<(), OrbitError> {
        let conn = orbit_common::fs::lock_wait::mutex(&self.conn)
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        // The run row and its state row commit together.
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(sqlite_store_error)?;
        upsert_job_run_for_workspace_conn(&tx, workspace_id, run, pipeline_state)?;
        tx.commit().map_err(sqlite_store_error)
    }

    /// Insert or replace every run, and its pipeline state, in one transaction.
    pub fn upsert_job_runs_for_workspace(
        &self,
        workspace_id: &str,
        runs: &[(&JobRun, Option<&PipelineState>)],
    ) -> Result<(), OrbitError> {
        if runs.is_empty() {
            return Ok(());
        }
        let conn = orbit_common::fs::lock_wait::mutex(&self.conn)
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)
                .map_err(sqlite_store_error)?;
        for (run, pipeline_state) in runs {
            upsert_job_run_for_workspace_conn(&tx, workspace_id, run, *pipeline_state)?;
        }
        tx.commit().map_err(sqlite_store_error)
    }

    pub fn upsert_job_run_step_for_workspace(
        &self,
        workspace_id: &str,
        run_id: &str,
        step: &JobRunStep,
    ) -> Result<(), OrbitError> {
        let agent_response_json = optional_json(&step.agent_response_json, "agent response")?;
        // The typed code's storage is the existing `error_code` column: a
        // step that carries only the typed form persists its string there.
        let error_code = step
            .error_code
            .clone()
            .or_else(|| step.failure_code.as_ref().map(ToString::to_string));
        let conn = orbit_common::fs::lock_wait::mutex(&self.conn)
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        conn.execute(
            r#"INSERT INTO job_run_steps(
                workspace_id, run_id, step_index, target_type, target_id, state,
                started_at, finished_at, duration_ms, exit_code, error_code,
                error_message, agent_response_json, lock_wait_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
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
                agent_response_json = excluded.agent_response_json,
                lock_wait_ms = excluded.lock_wait_ms"#,
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
                error_code,
                step.error_message,
                agent_response_json,
                step.lock_wait_ms.map(|value| value as i64),
            ],
        )
        .map_err(sqlite_store_error)?;
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

    /// Run rows for `run_ids` with no step read and only task-label input.
    ///
    /// One `run_id IN (...)` statement per chunk of [`STEP_RUN_ID_CHUNK`]
    /// ids. An id with no row is omitted. A row that cannot be decoded is
    /// omitted too, so one corrupt record does not blank the rest of the
    /// chunk. `input` keeps only [`JOB_RUN_TASK_INPUT_KEYS`].
    pub(in crate::driver::sqlite::job_run_store) fn job_runs_by_id_for_workspace(
        &self,
        workspace_id: &str,
        run_ids: &[String],
    ) -> Result<Vec<JobRun>, OrbitError> {
        if run_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.read()?;
        let mut runs = Vec::with_capacity(run_ids.len());
        for chunk in run_ids.chunks(STEP_RUN_ID_CHUNK) {
            let placeholders = (0..chunk.len())
                .map(|index| format!("?{}", index + 2))
                .collect::<Vec<_>>()
                .join(", ");
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {JOB_RUN_COLUMNS} FROM job_runs \
                     WHERE workspace_id = ?1 AND run_id IN ({placeholders})"
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
                params.iter().map(|param| param.as_ref()).collect();
            let mut rows = stmt
                .query(param_refs.as_slice())
                .map_err(sqlite_store_error)?;
            loop {
                match rows.next() {
                    Ok(Some(row)) => {
                        match row_to_job_run_with_input_keys(row, Some(JOB_RUN_TASK_INPUT_KEYS)) {
                            Ok(run) => runs.push(run),
                            Err(error) => {
                                orbit_common::tracing::warn!(
                                    target: "orbit.store.job_run",
                                    %error,
                                    "job run row skipped during batched read"
                                );
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(error) => return Err(sqlite_store_error(error)),
                }
            }
        }
        Ok(runs)
    }

    /// The one-column counterpart of [`Self::get_job_run_for_workspace`]:
    /// no run row, step rows, input or pipeline state.
    pub fn get_job_run_state_for_workspace(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Option<JobRunState>, OrbitError> {
        let conn = self.read()?;
        let state = conn
            .query_row(
                "SELECT state FROM job_runs WHERE workspace_id = ?1 AND run_id = ?2",
                rusqlite::params![workspace_id, run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(sqlite_store_error)?;
        state
            .map(|raw| {
                parse_job_run_state(&raw).map_err(|error| {
                    OrbitError::Store(format!("invalid job run state for {run_id}: {error}"))
                })
            })
            .transpose()
    }

    /// Every workspace id this store records `run_id` under, ascending,
    /// whether or not that workspace is still registered on the host. A run
    /// id is unique only within one workspace, so more than one can match.
    /// Each distinct workspace is probed through the `(workspace_id, run_id)`
    /// primary key rather than scanning run rows.
    pub fn job_run_workspace_ids(&self, run_id: &str) -> Result<Vec<String>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(
                "SELECT w.workspace_id FROM (SELECT DISTINCT workspace_id FROM job_runs) w \
                 WHERE EXISTS (SELECT 1 FROM job_runs r \
                               WHERE r.workspace_id = w.workspace_id AND r.run_id = ?1) \
                 ORDER BY w.workspace_id ASC",
            )
            .map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(rusqlite::params![run_id], |row| row.get::<_, String>(0))
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    pub fn list_job_runs_for_workspace(
        &self,
        workspace_id: &str,
        query: &JobRunQuery,
    ) -> Result<Vec<JobRun>, OrbitError> {
        let (sql, params) = job_run_list_sql(workspace_id, query);
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|b| b.as_ref()).collect();
        let conn = self.read()?;
        let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                row_to_job_run_with_input_keys(row, query.input_keys)
            })
            .map_err(sqlite_store_error)?;
        let mut runs = rows
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)?;
        drop(stmt);
        if query.include_steps {
            let run_ids = runs
                .iter()
                .map(|run| run.run_id.clone())
                .collect::<Vec<_>>();
            let mut steps_by_run = read_steps_for_runs(&conn, workspace_id, &run_ids)?;
            for run in &mut runs {
                run.steps = steps_by_run.remove(&run.run_id).unwrap_or_default();
            }
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
        .map_err(sqlite_store_error)
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
            .map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| row.get::<_, i64>(0))
            .map_err(sqlite_store_error)?;
        rows.map(|row| row.map(|value| value.max(0) as u64))
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    pub fn delete_job_run_for_workspace(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<bool, OrbitError> {
        let conn = orbit_common::fs::lock_wait::mutex(&self.conn)
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        conn.execute(
            "DELETE FROM job_runs WHERE workspace_id = ?1 AND run_id = ?2",
            rusqlite::params![workspace_id, run_id],
        )
        .map(|count| count > 0)
        .map_err(sqlite_store_error)
    }
}
