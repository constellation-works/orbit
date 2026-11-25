//! Pipeline-state rows: single and bulk reads, and the state-row writes.
//!
//! A run's pipeline state lives in `job_run_states`, a 1:1 side table of
//! `job_runs` (schema v38). Run listings never touch it, so they never walk
//! a checkpoint's overflow-page chain; a checkpoint write rewrites only the
//! state row, never the listing row.

use std::collections::HashMap;
use std::str::FromStr;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::{JobRunState, PipelineState};
use rusqlite::OptionalExtension;

use super::{JobRunRepository, STEP_RUN_ID_CHUNK};
use crate::store::Tx;

impl<Access> JobRunRepository<'_, Access> {
    /// The run's serialized pipeline state, unparsed and as stored; `None`
    /// when the run has none or does not exist.
    pub fn get_pipeline_state_json(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Option<String>, OrbitError> {
        read_state_json_conn(self.conn, workspace_id, run_id)
    }

    /// The run's pipeline state; `None` when the run has none or does not
    /// exist. Unreadable JSON fails with `invalid pipeline_state_json`.
    pub fn get_pipeline_state(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Option<PipelineState>, OrbitError> {
        read_state_json_conn(self.conn, workspace_id, run_id)?
            .map(|raw| {
                serde_json::from_str(&raw)
                    .map_err(|e| OrbitError::Store(format!("invalid pipeline_state_json: {e}")))
            })
            .transpose()
    }

    /// Serialized pipeline states for many runs, unparsed, in one query per
    /// chunk. A run with no state row, or no run, maps to `None`.
    pub fn get_pipeline_state_jsons(
        &self,
        workspace_id: &str,
        run_ids: &[String],
    ) -> Result<HashMap<String, Option<String>>, OrbitError> {
        let mut states: HashMap<String, Option<String>> = HashMap::new();
        if run_ids.is_empty() {
            return Ok(states);
        }
        for chunk in run_ids.chunks(STEP_RUN_ID_CHUNK) {
            let placeholders = (0..chunk.len())
                .map(|index| format!("?{}", index + 2))
                .collect::<Vec<_>>()
                .join(", ");
            let mut stmt = self
                .conn
                .prepare(&format!(
                    "SELECT run_id, pipeline_state_json FROM job_run_states \
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
                params.iter().map(|b| b.as_ref()).collect();
            let rows = stmt
                .query_map(param_refs.as_slice(), |row| {
                    let run_id: String = row.get(0)?;
                    let raw: String = row.get(1)?;
                    Ok((run_id, raw))
                })
                .map_err(sqlite_store_error)?;
            for row in rows {
                let (run_id, raw) = row.map_err(sqlite_store_error)?;
                states.insert(run_id, Some(raw));
            }
            for run_id in chunk {
                states.entry(run_id.clone()).or_insert(None);
            }
        }
        Ok(states)
    }

    /// Pipeline states for many runs in one query per chunk.
    ///
    /// Unreadable JSON is `None` for that run rather than failing the page:
    /// list projection already treats a per-run read error as empty lineage.
    /// A run that does not exist is absent.
    pub fn get_pipeline_states(
        &self,
        workspace_id: &str,
        run_ids: &[String],
    ) -> Result<HashMap<String, Option<PipelineState>>, OrbitError> {
        self.read_state_page(workspace_id, run_ids, false)
    }

    /// Pipeline states for many runs in one query per chunk, failing on
    /// unreadable JSON with `invalid pipeline_state_json`, the error of
    /// [`Self::get_pipeline_state`]. A run with no state row is `None`; a run
    /// that does not exist is absent.
    pub fn get_pipeline_states_exact(
        &self,
        workspace_id: &str,
        run_ids: &[String],
    ) -> Result<HashMap<String, Option<PipelineState>>, OrbitError> {
        self.read_state_page(workspace_id, run_ids, true)
    }

    /// The run's lifecycle state and its serialized pipeline state, in one
    /// read. `None` when the run does not exist.
    pub fn get_with_pipeline_state(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Option<(JobRunState, Option<String>)>, OrbitError> {
        let row = self
            .conn
            .query_row(
                "SELECT r.state, s.pipeline_state_json FROM job_runs r \
                 LEFT JOIN job_run_states s \
                   ON s.workspace_id = r.workspace_id AND s.run_id = r.run_id \
                 WHERE r.workspace_id = ?1 AND r.run_id = ?2",
                rusqlite::params![workspace_id, run_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .map_err(sqlite_store_error)?;
        row.map(|(raw_state, pipeline_state)| {
            let state = JobRunState::from_str(&raw_state)
                .map_err(|error| OrbitError::Store(format!("invalid job run state: {error}")))?;
            Ok((state, pipeline_state))
        })
        .transpose()
    }

    fn read_state_page(
        &self,
        workspace_id: &str,
        run_ids: &[String],
        exact: bool,
    ) -> Result<HashMap<String, Option<PipelineState>>, OrbitError> {
        let mut states: HashMap<String, Option<PipelineState>> = HashMap::new();
        if run_ids.is_empty() {
            return Ok(states);
        }
        for chunk in run_ids.chunks(STEP_RUN_ID_CHUNK) {
            let placeholders = (0..chunk.len())
                .map(|index| format!("?{}", index + 2))
                .collect::<Vec<_>>()
                .join(", ");
            let mut stmt = self
                .conn
                .prepare(&format!(
                    "SELECT r.run_id, s.pipeline_state_json FROM job_runs r \
                     LEFT JOIN job_run_states s \
                       ON s.workspace_id = r.workspace_id AND s.run_id = r.run_id \
                     WHERE r.workspace_id = ?1 AND r.run_id IN ({placeholders})"
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
                    let raw: Option<String> = row.get(1)?;
                    Ok((run_id, raw))
                })
                .map_err(sqlite_store_error)?;
            for row in rows {
                let (run_id, raw) = row.map_err(sqlite_store_error)?;
                let state = match raw {
                    None => None,
                    Some(raw) => match serde_json::from_str(&raw) {
                        Ok(state) => Some(state),
                        Err(error) if exact => {
                            return Err(OrbitError::Store(format!(
                                "invalid pipeline_state_json: {error}"
                            )));
                        }
                        Err(_) => None,
                    },
                };
                states.insert(run_id, state);
            }
        }
        Ok(states)
    }
}

impl JobRunRepository<'_, Tx<'_>> {
    /// Insert or replace the run's serialized pipeline state. `false` when
    /// the run does not exist: no state row is ever written without its run.
    pub fn put_pipeline_state_json(
        &self,
        workspace_id: &str,
        run_id: &str,
        state_json: &str,
    ) -> Result<bool, OrbitError> {
        write_state_json_conn(self.conn, workspace_id, run_id, state_json)
    }

    /// Insert the run's serialized pipeline state only while it has none.
    /// `false` when a state row already exists or the run does not exist;
    /// the stored document is left as it is.
    pub fn put_pipeline_state_json_if_absent(
        &self,
        workspace_id: &str,
        run_id: &str,
        state_json: &str,
    ) -> Result<bool, OrbitError> {
        self.conn
            .execute(
                "INSERT INTO job_run_states(workspace_id, run_id, pipeline_state_json) \
                 SELECT ?1, ?2, ?3 WHERE EXISTS(\
                     SELECT 1 FROM job_runs WHERE workspace_id = ?1 AND run_id = ?2) \
                 ON CONFLICT(workspace_id, run_id) DO NOTHING",
                rusqlite::params![workspace_id, run_id, state_json],
            )
            .map(|inserted| inserted != 0)
            .map_err(sqlite_store_error)
    }
}

/// The run's serialized pipeline state, or `None` when the run has none or
/// does not exist.
fn read_state_json_conn(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    run_id: &str,
) -> Result<Option<String>, OrbitError> {
    conn.query_row(
        "SELECT pipeline_state_json FROM job_run_states WHERE workspace_id = ?1 AND run_id = ?2",
        rusqlite::params![workspace_id, run_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(sqlite_store_error)
}

/// Insert or replace the run's serialized pipeline state. `false` when the
/// run does not exist: no state row is ever written without its run.
pub(super) fn write_state_json_conn(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    run_id: &str,
    state_json: &str,
) -> Result<bool, OrbitError> {
    conn.execute(
        "INSERT INTO job_run_states(workspace_id, run_id, pipeline_state_json) \
         SELECT ?1, ?2, ?3 WHERE EXISTS(\
             SELECT 1 FROM job_runs WHERE workspace_id = ?1 AND run_id = ?2) \
         ON CONFLICT(workspace_id, run_id) DO UPDATE SET \
             pipeline_state_json = excluded.pipeline_state_json",
        rusqlite::params![workspace_id, run_id, state_json],
    )
    .map(|changed| changed > 0)
    .map_err(sqlite_store_error)
}
