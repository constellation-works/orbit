//! Pipeline-state read, bulk-read, write, and immediate read-modify-write.
//!
//! A run's pipeline state lives in `job_run_states`, a 1:1 side table of
//! `job_runs` (schema v38). Run listings never touch it, so they never walk
//! a checkpoint's overflow-page chain; a checkpoint write rewrites only the
//! state row, never the listing row.

use std::collections::HashMap;
use std::str::FromStr;

use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::workflow::{JobRunState, PipelineState, RunStateUpdate};
use rusqlite::{OptionalExtension, TransactionBehavior};

use super::queries::STEP_RUN_ID_CHUNK;
use crate::Store;

impl Store {
    pub fn read_job_run_state_for_workspace(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Option<PipelineState>, OrbitError> {
        let conn = self.read()?;
        let raw = read_state_json_conn(&conn, workspace_id, run_id)?;
        raw.map(|raw| {
            serde_json::from_str(&raw)
                .map_err(|e| OrbitError::Store(format!("invalid pipeline_state_json: {e}")))
        })
        .transpose()
    }

    /// Pipeline state for a page of runs in one query per chunk.
    ///
    /// Unreadable JSON is `None` for that run rather than failing the page:
    /// list projection already treats a per-run read error as empty lineage.
    /// A caller that must keep [`Self::read_job_run_state_for_workspace`]'s
    /// error uses [`Self::read_job_run_states_exact_for_workspace`].
    pub fn read_job_run_states_for_workspace(
        &self,
        workspace_id: &str,
        run_ids: &[String],
    ) -> Result<HashMap<String, Option<PipelineState>>, OrbitError> {
        self.read_job_run_state_page(workspace_id, run_ids, false)
    }

    /// Pipeline state for many runs in one query per chunk.
    ///
    /// Unreadable JSON fails the call with `invalid pipeline_state_json`, the
    /// same error as [`Self::read_job_run_state_for_workspace`]. A run with no
    /// state row is `None`.
    pub fn read_job_run_states_exact_for_workspace(
        &self,
        workspace_id: &str,
        run_ids: &[String],
    ) -> Result<HashMap<String, Option<PipelineState>>, OrbitError> {
        self.read_job_run_state_page(workspace_id, run_ids, true)
    }

    fn read_job_run_state_page(
        &self,
        workspace_id: &str,
        run_ids: &[String],
        exact: bool,
    ) -> Result<HashMap<String, Option<PipelineState>>, OrbitError> {
        let mut states: HashMap<String, Option<PipelineState>> = HashMap::new();
        if run_ids.is_empty() {
            return Ok(states);
        }
        let conn = self.read()?;
        for chunk in run_ids.chunks(STEP_RUN_ID_CHUNK) {
            let placeholders = (0..chunk.len())
                .map(|index| format!("?{}", index + 2))
                .collect::<Vec<_>>()
                .join(", ");
            let mut stmt = conn
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

    pub fn write_job_run_state_for_workspace(
        &self,
        workspace_id: &str,
        run_id: &str,
        state: &PipelineState,
    ) -> Result<(), OrbitError> {
        state
            .checkpoint_write_supported()
            .map_err(OrbitError::Store)?;
        let conn = orbit_common::fs::lock_wait::mutex(&self.conn)
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        // [ORB-20158] Never overwrite a checkpoint a newer binary wrote.
        if let Some(raw) = read_state_json_conn(&conn, workspace_id, run_id)? {
            refuse_unsupported_stored_checkpoint(&raw)?;
        }
        // A snapshot written after the run finished must not bring back what
        // finalization compacted away.
        let compacted = run_state_conn(&conn, workspace_id, run_id)?.and_then(|run_state| {
            let mut compacted = state.clone();
            compacted
                .compact_for_terminal(run_state)
                .then_some(compacted)
        });
        let state_json = serde_json::to_string(compacted.as_ref().unwrap_or(state))
            .map_err(|e| OrbitError::Store(format!("serialize pipeline state: {e}")))?;
        if !write_state_json_conn(&conn, workspace_id, run_id, &state_json)? {
            return Err(OrbitError::not_found(
                NotFoundKind::JobRun,
                run_id.to_string(),
            ));
        }
        Ok(())
    }

    /// Initialize without replacing a checkpoint another writer supplied.
    pub fn initialize_job_run_state_for_workspace(
        &self,
        workspace_id: &str,
        run_id: &str,
        state: &PipelineState,
    ) -> Result<bool, OrbitError> {
        let state_json = serde_json::to_string(state)
            .map_err(|e| OrbitError::Store(format!("serialize pipeline state: {e}")))?;
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let updated = tx
                .tx
                .execute(
                    "INSERT INTO job_run_states(workspace_id, run_id, pipeline_state_json) \
                     SELECT ?1, ?2, ?3 WHERE EXISTS(\
                         SELECT 1 FROM job_runs WHERE workspace_id = ?1 AND run_id = ?2) \
                     ON CONFLICT(workspace_id, run_id) DO NOTHING",
                    rusqlite::params![workspace_id, run_id, state_json],
                )
                .map_err(sqlite_store_error)?;
            if updated == 0 {
                let exists = tx.tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM job_runs WHERE workspace_id = ?1 AND run_id = ?2)",
                    rusqlite::params![workspace_id, run_id],
                    |row| row.get::<_, bool>(0),
                ).map_err(sqlite_store_error)?;
                if !exists {
                    return Err(OrbitError::not_found(
                        NotFoundKind::JobRun,
                        run_id.to_string(),
                    ));
                }
            }
            Ok(updated != 0)
        })
    }

    /// [ORB-11253] Apply `update` to a run's pipeline state, reading the run's
    /// state and its checkpoint blob inside the same immediate write
    /// transaction that persists the result.
    ///
    /// `IMMEDIATE` takes the write lock at BEGIN rather than at first write, so
    /// two callers serialize here instead of racing between their own read and
    /// write. An `Err` from `update` — the shape both a refused terminal run
    /// and a lost compare-and-set take — propagates before the commit, leaving
    /// the stored state untouched. A finished run's result stays compacted
    /// (`PipelineState::compact_for_terminal`), so a late checkpoint cannot
    /// restore its resume-only maps.
    pub fn update_job_run_state_for_workspace(
        &self,
        workspace_id: &str,
        run_id: &str,
        update: &mut dyn FnMut(JobRunState, &mut PipelineState) -> Result<(), OrbitError>,
    ) -> Result<RunStateUpdate, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let row = tx
                .tx
                .query_row(
                    "SELECT r.state, s.pipeline_state_json FROM job_runs r \
                     LEFT JOIN job_run_states s \
                       ON s.workspace_id = r.workspace_id AND s.run_id = r.run_id \
                     WHERE r.workspace_id = ?1 AND r.run_id = ?2",
                    rusqlite::params![workspace_id, run_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .map(Some)
                .or_else(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(sqlite_store_error(other)),
                })?;
            let Some((raw_state, raw_pipeline_state)) = row else {
                return Ok(RunStateUpdate::NotFound);
            };
            let state = JobRunState::from_str(&raw_state)
                .map_err(|error| OrbitError::Store(format!("invalid job run state: {error}")))?;
            let Some(raw_pipeline_state) = raw_pipeline_state else {
                return Ok(RunStateUpdate::NoState);
            };
            refuse_unsupported_stored_checkpoint(&raw_pipeline_state)?;
            let mut pipeline_state: PipelineState = serde_json::from_str(&raw_pipeline_state)
                .map_err(|e| OrbitError::Store(format!("invalid pipeline_state_json: {e}")))?;
            update(state, &mut pipeline_state)?;
            pipeline_state
                .checkpoint_write_supported()
                .map_err(OrbitError::Store)?;
            pipeline_state.compact_for_terminal(state);
            let state_json = serde_json::to_string(&pipeline_state)
                .map_err(|e| OrbitError::Store(format!("serialize pipeline state: {e}")))?;
            tx.tx
                .execute(
                    "UPDATE job_run_states SET pipeline_state_json = ?3 \
                     WHERE workspace_id = ?1 AND run_id = ?2",
                    rusqlite::params![workspace_id, run_id, state_json],
                )
                .map_err(sqlite_store_error)?;
            Ok(RunStateUpdate::Updated)
        })
    }
}

impl Store {
    /// [ORB-20158] See `JobRunStore::adopt_parked_job_run`: the run row's
    /// owner and the state's park change in one immediate transaction.
    pub fn adopt_parked_job_run_for_workspace(
        &self,
        workspace_id: &str,
        run_id: &str,
        parked_at: chrono::DateTime<chrono::Utc>,
        pid: u32,
    ) -> Result<bool, OrbitError> {
        let pid_start_time = orbit_common::process::identity::process_start_identity_token(pid);
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let Some(mut run) =
                super::queries::get_job_run_for_workspace_conn(&tx.tx, workspace_id, run_id)?
            else {
                return Ok(false);
            };
            if run.state != JobRunState::Running {
                return Ok(false);
            }
            let Some(raw) = read_state_json_conn(&tx.tx, workspace_id, run_id)? else {
                return Ok(false);
            };
            refuse_unsupported_stored_checkpoint(&raw)?;
            let mut state: PipelineState = serde_json::from_str(&raw)
                .map_err(|e| OrbitError::Store(format!("invalid pipeline_state_json: {e}")))?;
            let Some(checkpoint) = state.auto_task.as_mut() else {
                return Ok(false);
            };
            if checkpoint.parked.as_ref().map(|park| park.parked_at) != Some(parked_at) {
                return Ok(false);
            }
            checkpoint.parked = None;
            state.updated_at = chrono::Utc::now();
            let state_json = serde_json::to_string(&state)
                .map_err(|e| OrbitError::Store(format!("serialize pipeline state: {e}")))?;
            write_state_json_conn(&tx.tx, workspace_id, run_id, &state_json)?;
            run.pid = Some(pid);
            run.pid_start_time = pid_start_time;
            super::queries::upsert_job_run_for_workspace_conn(&tx.tx, workspace_id, &run, None)?;
            Ok(true)
        })
    }
}

/// [ORB-20158] Refuse a stored state whose auto-task checkpoint a newer
/// binary wrote: rewriting it through this binary's shape could drop pending
/// bindings, receipts or the parked continuation. Read before deserializing,
/// so the refusal does not depend on the newer shape still parsing.
fn refuse_unsupported_stored_checkpoint(raw: &str) -> Result<(), OrbitError> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(());
    };
    let Some(version) = value
        .get("auto_task")
        .and_then(|checkpoint| checkpoint.get("version"))
    else {
        return Ok(());
    };
    match version.as_u64() {
        Some(version)
            if version <= u64::from(orbit_types::workflow::AUTO_TASK_CHECKPOINT_VERSION) =>
        {
            Ok(())
        }
        _ => Err(OrbitError::Store(format!(
            "auto-task checkpoint version {version} is newer than the supported {}; this Orbit \
             refuses to rewrite the run state",
            orbit_types::workflow::AUTO_TASK_CHECKPOINT_VERSION
        ))),
    }
}

/// The run's serialized pipeline state, or `None` when the run has none or
/// does not exist.
pub(super) fn read_state_json_conn(
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

/// The run's lifecycle state, or `None` when the run does not exist.
fn run_state_conn(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    run_id: &str,
) -> Result<Option<JobRunState>, OrbitError> {
    conn.query_row(
        "SELECT state FROM job_runs WHERE workspace_id = ?1 AND run_id = ?2",
        rusqlite::params![workspace_id, run_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(sqlite_store_error)?
    .map(|raw| {
        JobRunState::from_str(&raw)
            .map_err(|error| OrbitError::Store(format!("invalid job run state: {error}")))
    })
    .transpose()
}

/// [ORB-14587] Drop the resume-only maps of a run that just finished
/// `run_state`, inside the transaction that finished it. Best-effort: an
/// unreadable checkpoint is left as it is rather than failing the terminal
/// write.
pub(super) fn compact_finished_state_conn(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    run_id: &str,
    run_state: JobRunState,
) -> Result<(), OrbitError> {
    let Some(raw) = read_state_json_conn(conn, workspace_id, run_id)? else {
        return Ok(());
    };
    if refuse_unsupported_stored_checkpoint(&raw).is_err() {
        return Ok(());
    }
    let mut state = match serde_json::from_str::<PipelineState>(&raw) {
        Ok(state) => state,
        Err(error) => {
            orbit_common::tracing::warn!(
                run_id,
                %error,
                "finished run's pipeline state is unreadable; left uncompacted"
            );
            return Ok(());
        }
    };
    if !state.compact_for_terminal(run_state) {
        return Ok(());
    }
    let state_json = serde_json::to_string(&state)
        .map_err(|e| OrbitError::Store(format!("serialize pipeline state: {e}")))?;
    write_state_json_conn(conn, workspace_id, run_id, &state_json).map(|_| ())
}
