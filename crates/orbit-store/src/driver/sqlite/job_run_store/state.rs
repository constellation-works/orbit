//! Pipeline-state read, bulk-read, write, and immediate read-modify-write.

use std::collections::HashMap;
use std::str::FromStr;

use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::workflow::{JobRunState, PipelineState, RunStateUpdate};
use rusqlite::TransactionBehavior;

use super::queries::STEP_RUN_ID_CHUNK;
use crate::Store;

impl Store {
    pub fn read_job_run_state_for_workspace(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Option<PipelineState>, OrbitError> {
        let conn = self.read()?;
        let raw = match conn.query_row(
            "SELECT pipeline_state_json FROM job_runs WHERE workspace_id = ?1 AND run_id = ?2",
            rusqlite::params![workspace_id, run_id],
            |row| row.get::<_, Option<String>>(0),
        ) {
            Ok(raw) => raw,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(err) => return Err(OrbitError::Store(err.to_string())),
        };
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
    pub fn read_job_run_states_for_workspace(
        &self,
        workspace_id: &str,
        run_ids: &[String],
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
                    "SELECT run_id, pipeline_state_json FROM job_runs \
                     WHERE workspace_id = ?1 AND run_id IN ({placeholders})"
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
                    let raw: Option<String> = row.get(1)?;
                    Ok((run_id, raw))
                })
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            for row in rows {
                let (run_id, raw) = row.map_err(|e| OrbitError::Store(e.to_string()))?;
                let state = raw.and_then(|raw| serde_json::from_str(&raw).ok());
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
        let state_json = serde_json::to_string_pretty(state)
            .map_err(|e| OrbitError::Store(format!("serialize pipeline state: {e}")))?;
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let updated = conn
            .execute(
                "UPDATE job_runs SET pipeline_state_json = ?3 WHERE workspace_id = ?1 AND run_id = ?2",
                rusqlite::params![workspace_id, run_id, state_json],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        if updated == 0 {
            return Err(OrbitError::not_found(
                NotFoundKind::JobRun,
                run_id.to_string(),
            ));
        }
        Ok(())
    }

    /// [ORB-11253] Apply `update` to a run's pipeline state, reading the run's
    /// state and its checkpoint blob inside the same immediate write
    /// transaction that persists the result.
    ///
    /// `IMMEDIATE` takes the write lock at BEGIN rather than at first write, so
    /// two callers serialize here instead of racing between their own read and
    /// write. An `Err` from `update` — the shape both a refused terminal run
    /// and a lost compare-and-set take — propagates before the commit, leaving
    /// the stored state untouched.
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
                    "SELECT state, pipeline_state_json FROM job_runs \
                     WHERE workspace_id = ?1 AND run_id = ?2",
                    rusqlite::params![workspace_id, run_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                )
                .map(Some)
                .or_else(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(OrbitError::Store(other.to_string())),
                })?;
            let Some((raw_state, raw_pipeline_state)) = row else {
                return Ok(RunStateUpdate::NotFound);
            };
            let state = JobRunState::from_str(&raw_state)
                .map_err(|error| OrbitError::Store(format!("invalid job run state: {error}")))?;
            let Some(raw_pipeline_state) = raw_pipeline_state else {
                return Ok(RunStateUpdate::NoState);
            };
            let mut pipeline_state: PipelineState = serde_json::from_str(&raw_pipeline_state)
                .map_err(|e| OrbitError::Store(format!("invalid pipeline_state_json: {e}")))?;
            update(state, &mut pipeline_state)?;
            let state_json = serde_json::to_string_pretty(&pipeline_state)
                .map_err(|e| OrbitError::Store(format!("serialize pipeline state: {e}")))?;
            tx.tx
                .execute(
                    "UPDATE job_runs SET pipeline_state_json = ?3 \
                     WHERE workspace_id = ?1 AND run_id = ?2",
                    rusqlite::params![workspace_id, run_id, state_json],
                )
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            Ok(RunStateUpdate::Updated)
        })
    }
}
