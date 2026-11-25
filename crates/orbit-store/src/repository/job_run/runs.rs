//! Run and step rows: reads on any access, writes on a [`Tx`] only.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::{
    JobRun, JobRunState, JobRunStep, OrbitBuild, PipelineState, RunIdRole,
};
use rusqlite::OptionalExtension;

use super::JobRunRepository;
use super::list_sql::{JOB_RUN_COLUMNS, job_run_filter_sql, job_run_list_sql, latest_job_runs_sql};
use super::rows::{
    get_job_run_for_workspace_conn, optional_json, parse_job_run_state, read_steps_for_runs,
    row_to_job_run, row_to_job_run_with_input_keys,
};
use super::write::{next_run_id_conn, upsert_job_run_for_workspace_conn};
use super::{LATEST_JOB_ID_CHUNK, STEP_RUN_ID_CHUNK};
use crate::contracts::{JOB_RUN_TASK_INPUT_KEYS, JobRunQuery};
use crate::store::Tx;

/// The window read behind [`JobRunRepository::list_keyed_window`]: one job's
/// runs, newest first, in the order `idx_job_runs_ws_job_created` stores them.
const KEYED_RUN_WINDOW_SQL: &str = "SELECT run_id, input_json FROM job_runs \
     WHERE workspace_id = ?1 AND job_id = ?2 ORDER BY created_at DESC, run_id ASC LIMIT ?3";

impl<Access> JobRunRepository<'_, Access> {
    /// One run of `workspace_id`, with its steps.
    pub fn get(&self, workspace_id: &str, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        get_job_run_for_workspace_conn(self.conn, workspace_id, run_id)
    }

    /// Whether `run_id` has a row in `workspace_id`.
    pub fn exists(&self, workspace_id: &str, run_id: &str) -> Result<bool, OrbitError> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM job_runs WHERE workspace_id = ?1 AND run_id = ?2)",
                rusqlite::params![workspace_id, run_id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(sqlite_store_error)
    }

    /// Only the run's lifecycle state: no step rows, no input and no
    /// pipeline state.
    pub fn get_state(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Option<JobRunState>, OrbitError> {
        let state = self
            .conn
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

    /// Steps-free run rows for `run_ids`, each `input` keeping only the
    /// task-label keys `task_id` and `task_ids`.
    ///
    /// One `run_id IN (...)` statement per chunk of 500 ids (the step
    /// hydration chunk); `job_run_steps` is never selected. A repeated id is
    /// read once. An id with no row is omitted, and so is a row that cannot
    /// be decoded, so one corrupt record does not blank the rest of the chunk.
    /// No stale run is reconciled.
    pub fn get_many(
        &self,
        workspace_id: &str,
        run_ids: &[String],
    ) -> Result<Vec<JobRun>, OrbitError> {
        if run_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut runs = Vec::with_capacity(run_ids.len());
        for chunk in run_ids.chunks(STEP_RUN_ID_CHUNK) {
            let placeholders = (0..chunk.len())
                .map(|index| format!("?{}", index + 2))
                .collect::<Vec<_>>()
                .join(", ");
            let mut stmt = self
                .conn
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

    /// Every workspace id this store records `run_id` under, ascending,
    /// whether or not that workspace is still registered on the host. A run
    /// id is unique only within one workspace, so more than one can match.
    /// Each distinct workspace is probed through the `(workspace_id, run_id)`
    /// primary key rather than scanning run rows.
    pub fn list_workspace_ids(&self, run_id: &str) -> Result<Vec<String>, OrbitError> {
        let mut stmt = self
            .conn
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

    /// Runs matching `query`, with steps when `query.include_steps`.
    pub fn list(&self, workspace_id: &str, query: &JobRunQuery) -> Result<Vec<JobRun>, OrbitError> {
        let (sql, params) = job_run_list_sql(workspace_id, query);
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|b| b.as_ref()).collect();
        let mut stmt = self.conn.prepare(&sql).map_err(sqlite_store_error)?;
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
            let mut steps_by_run = read_steps_for_runs(self.conn, workspace_id, &run_ids)?;
            for run in &mut runs {
                run.steps = steps_by_run.remove(&run.run_id).unwrap_or_default();
            }
        }
        Ok(runs)
    }

    /// `COUNT(*)` over the same filter [`Self::list`] applies, ignoring
    /// `limit`: a tile that only needs a number must not hydrate (and
    /// silently cap at) a page of rows to get it.
    pub fn count(&self, workspace_id: &str, query: &JobRunQuery) -> Result<u64, OrbitError> {
        let (where_clause, params) = job_run_filter_sql(workspace_id, query);
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|b| b.as_ref()).collect();
        self.conn
            .query_row(
                &format!("SELECT COUNT(*) FROM job_runs WHERE {where_clause}"),
                param_refs.as_slice(),
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count.max(0) as u64)
            .map_err(sqlite_store_error)
    }

    /// Every recorded `duration_ms` matching the filter, ignoring `limit`.
    /// Feeds percentile baselines without materializing whole runs.
    pub fn list_durations(
        &self,
        workspace_id: &str,
        query: &JobRunQuery,
    ) -> Result<Vec<u64>, OrbitError> {
        let (where_clause, params) = job_run_filter_sql(workspace_id, query);
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|b| b.as_ref()).collect();
        let mut stmt = self
            .conn
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

    /// Distinct trimmed, nonblank task IDs carried by the selected runs'
    /// top-level `task_ids` string array or text `task_id`.
    ///
    /// Run filters and limits apply before projection; nested fields confer
    /// no ownership. Only the ownership bindings are read, never run rows,
    /// checkpoints or steps. Selection has no age window unless the caller
    /// supplies one in `query`.
    pub fn list_task_ids(
        &self,
        workspace_id: &str,
        query: &JobRunQuery,
    ) -> Result<BTreeSet<String>, OrbitError> {
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
        let mut statement = self.conn.prepare(&sql).map_err(sqlite_store_error)?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                row.get::<_, String>(0)
            })
            .map_err(sqlite_store_error)?;
        let mut ids = BTreeSet::new();
        for row in rows {
            let id = row.map_err(sqlite_store_error)?;
            let id = id.trim();
            if !id.is_empty() {
                ids.insert(id.to_string());
            }
        }
        Ok(ids)
    }

    /// Latest created run, including steps, for each distinct requested job.
    /// Missing jobs are omitted; results are ordered by job ID. Ties use the
    /// same run-ID ordering as a default `JobRunQuery`. No older run history
    /// is decoded.
    pub fn list_latest(
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
        let mut runs = Vec::new();
        for chunk in ids.chunks(LATEST_JOB_ID_CHUNK) {
            let sql = latest_job_runs_sql(chunk.len());
            let mut params: Vec<&dyn rusqlite::types::ToSql> = vec![&workspace_id];
            params.extend(chunk.iter().map(|id| *id as &dyn rusqlite::types::ToSql));
            let mut stmt = self.conn.prepare(&sql).map_err(sqlite_store_error)?;
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
        let mut steps = read_steps_for_runs(self.conn, workspace_id, &run_ids)?;
        for run in &mut runs {
            run.steps = steps.remove(&run.run_id).unwrap_or_default();
        }
        runs.sort_by(|a, b| a.job_id.cmp(&b.job_id));
        Ok(runs)
    }

    /// Up to `per_task_limit` recent runs of `job_id` for each requested
    /// task, ordered by recency descending, then run ID ascending, including
    /// steps. See `super::task_runs`.
    pub fn list_recent_for_tasks(
        &self,
        workspace_id: &str,
        job_id: &str,
        task_ids: &[String],
        per_task_limit: usize,
    ) -> Result<BTreeMap<String, Vec<JobRun>>, OrbitError> {
        super::task_runs::recent_job_runs_for_tasks(
            self.conn,
            workspace_id,
            job_id,
            task_ids,
            per_task_limit,
        )
    }

    /// Exact retry children of `run_id`, oldest first, with steps; at most
    /// `limit` (capped at 1000).
    pub fn list_retries(
        &self,
        workspace_id: &str,
        run_id: &str,
        limit: usize,
    ) -> Result<Vec<JobRun>, OrbitError> {
        let mut statement = self
            .conn
            .prepare(&format!(
                "SELECT {JOB_RUN_COLUMNS} FROM job_runs \
                 WHERE workspace_id=?1 AND retry_source_run_id=?2 \
                 ORDER BY created_at,run_id LIMIT ?3"
            ))
            .map_err(sqlite_store_error)?;
        let mut runs = statement
            .query_map(
                rusqlite::params![workspace_id, run_id, limit.min(1000)],
                row_to_job_run,
            )
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)?;
        drop(statement);
        // One step read for every child instead of one run read (and one
        // step read) each.
        let ids = runs
            .iter()
            .map(|run| run.run_id.clone())
            .collect::<Vec<_>>();
        let mut steps = read_steps_for_runs(self.conn, workspace_id, &ids)?;
        for run in &mut runs {
            run.steps = steps.remove(&run.run_id).unwrap_or_default();
        }
        Ok(runs)
    }

    /// The oldest non-terminal (`pending`, `running` or `retrying`) run in
    /// `source_run_id`'s retry lineage: its ancestors plus every run descended
    /// from any of them.
    ///
    /// Both walks are unbounded in depth, because a truncated ancestor walk
    /// would miss a live sibling of an older ancestor. Each recursive row
    /// carries only the run's own columns, so `UNION` discards a revisited run
    /// and a corrupted `retry_source_run_id` cycle terminates after visiting
    /// each run once.
    pub fn get_live_in_lineage(
        &self,
        workspace_id: &str,
        source_run_id: &str,
    ) -> Result<Option<String>, OrbitError> {
        self.conn
            .query_row(
                "WITH RECURSIVE \
                   ancestors(run_id, parent) AS ( \
                     SELECT run_id, retry_source_run_id FROM job_runs \
                      WHERE workspace_id = ?1 AND run_id = ?2 \
                     UNION \
                     SELECT j.run_id, j.retry_source_run_id \
                       FROM job_runs j JOIN ancestors a ON j.run_id = a.parent \
                      WHERE j.workspace_id = ?1 \
                   ), \
                   lineage(run_id) AS ( \
                     SELECT run_id FROM ancestors \
                     UNION \
                     SELECT j.run_id FROM job_runs j JOIN lineage l ON j.retry_source_run_id = l.run_id \
                      WHERE j.workspace_id = ?1 \
                   ) \
                 SELECT j.run_id FROM job_runs j JOIN lineage l ON j.run_id = l.run_id \
                  WHERE j.workspace_id = ?1 AND j.state IN ('pending', 'running', 'retrying') \
                  ORDER BY j.created_at, j.run_id LIMIT 1",
                rusqlite::params![workspace_id, source_run_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(sqlite_store_error)
    }

    /// `job_id`'s newest `limit` runs, newest first (the run list's default
    /// `created_at DESC, run_id ASC` order), as `(run_id, input_json)`.
    pub fn list_keyed_window(
        &self,
        workspace_id: &str,
        job_id: &str,
        limit: usize,
    ) -> Result<Vec<(String, Option<String>)>, OrbitError> {
        let mut statement = self
            .conn
            .prepare(KEYED_RUN_WINDOW_SQL)
            .map_err(sqlite_store_error)?;
        let rows = statement
            .query_map(
                rusqlite::params![
                    workspace_id,
                    job_id,
                    i64::try_from(limit).unwrap_or(i64::MAX)
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .map_err(sqlite_store_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    /// The run bound to the automation action `key`, if any.
    pub fn get_automation_key(
        &self,
        workspace_id: &str,
        key: &str,
    ) -> Result<Option<String>, OrbitError> {
        self.conn
            .query_row(
                "SELECT run_id FROM automation_job_keys WHERE workspace_id=?1 AND action_key=?2",
                rusqlite::params![workspace_id, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_store_error)
    }

    /// Every `pending`, `running` or `retrying` run as `(run_id, job_id,
    /// pipeline_state_json)`, with no step, input or run-row decoding.
    pub fn list_active_with_pipeline_state(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<(String, String, Option<String>)>, OrbitError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT r.run_id, r.job_id, s.pipeline_state_json FROM job_runs r \
                 LEFT JOIN job_run_states s ON s.workspace_id = r.workspace_id AND s.run_id = r.run_id \
                 WHERE r.workspace_id=?1 AND r.state IN ('pending','running','retrying')",
            )
            .map_err(sqlite_store_error)?;
        stmt.query_map([workspace_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(sqlite_store_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
    }
}

impl JobRunRepository<'_, Tx<'_>> {
    /// Allocate the next free run id of `role` for the minute `submitted_at`
    /// falls in, across every workspace in the host's shared store. See
    /// `super::write`. Insert the run in the same transaction.
    pub fn next_id(
        &self,
        role: RunIdRole,
        submitted_at: DateTime<Utc>,
    ) -> Result<String, OrbitError> {
        next_run_id_conn(self.conn, role, submitted_at)
    }

    /// Insert or replace a run row and reserve its id for good. A
    /// `pipeline_state` replaces the run's state row; `None` keeps whatever
    /// state the run already has. The run's steps are not written.
    pub fn put(
        &self,
        workspace_id: &str,
        run: &JobRun,
        pipeline_state: Option<&PipelineState>,
    ) -> Result<(), OrbitError> {
        upsert_job_run_for_workspace_conn(self.conn, workspace_id, run, pipeline_state)
    }

    /// Replace a run's recorded build (`build_json`) alone.
    pub fn put_build(
        &self,
        workspace_id: &str,
        run_id: &str,
        build: &OrbitBuild,
    ) -> Result<(), OrbitError> {
        let build = serde_json::to_string(build)
            .map_err(|error| OrbitError::Store(format!("serialize Orbit build: {error}")))?;
        self.conn
            .execute(
                "UPDATE job_runs SET build_json = ?3 WHERE workspace_id = ?1 AND run_id = ?2",
                rusqlite::params![workspace_id, run_id, build],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }

    /// Insert or replace one step row of `run_id`.
    pub fn put_step(
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
        self.conn
            .execute(
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

    /// Delete a run row. `false` when there was none. The id stays reserved
    /// in `job_run_id_allocations`.
    pub fn delete(&self, workspace_id: &str, run_id: &str) -> Result<bool, OrbitError> {
        self.conn
            .execute(
                "DELETE FROM job_runs WHERE workspace_id = ?1 AND run_id = ?2",
                rusqlite::params![workspace_id, run_id],
            )
            .map(|count| count > 0)
            .map_err(sqlite_store_error)
    }

    /// Bind the automation action `key` to `run_id`.
    pub fn put_automation_key(
        &self,
        workspace_id: &str,
        key: &str,
        run_id: &str,
    ) -> Result<(), OrbitError> {
        self.conn
            .execute(
                "INSERT INTO automation_job_keys VALUES (?1,?2,?3)",
                rusqlite::params![workspace_id, key, run_id],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }
}
