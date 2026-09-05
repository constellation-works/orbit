use std::collections::HashMap;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use orbit_common::process::identity::process_start_identity_token;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::identity::Crew;
use orbit_types::workflow::{
    ChildDispatch, JobRun, JobRunStartOutcome, JobRunState, JobRunStep, JobTargetType,
    KnowledgeRunMetrics, PipelineState, RunEvent, RunStateUpdate,
};
use rusqlite::{OptionalExtension, TransactionBehavior};

use crate::contracts::{
    ChildJobRunAdmissionOutcome, ChildJobRunAdmissionParams, JobRunOrder, JobRunQuery,
    JobRunStepParams, JobRunStoreBackend,
};
use crate::fs::path_safety::validate_path_stem;
use crate::{Store, parse_timestamp};

#[derive(Clone)]
pub struct SqliteJobRunStore {
    store: Store,
    workspace_id: String,
}

impl SqliteJobRunStore {
    pub fn new(store: Store, workspace_id: impl Into<String>) -> Self {
        Self {
            store,
            workspace_id: workspace_id.into(),
        }
    }

    fn read_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        self.store
            .get_job_run_for_workspace(&self.workspace_id, run_id)
    }

    fn update_run(
        &self,
        run_id: &str,
        update: impl FnOnce(&mut JobRun) -> Result<(), OrbitError>,
    ) -> Result<bool, OrbitError> {
        self.store
            .with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
                let Some(mut run) =
                    get_job_run_for_workspace_conn(&tx.tx, &self.workspace_id, run_id)?
                else {
                    return Ok(false);
                };
                update(&mut run)?;
                upsert_job_run_for_workspace_conn(&tx.tx, &self.workspace_id, &run, None)?;
                Ok(true)
            })
    }

    fn next_run_id(&self, job_id: &str) -> Result<String, OrbitError> {
        let base = format!("jrun-{}", Utc::now().format("%Y%m%d-%H%M"));
        for suffix in 1..1024_u32 {
            let candidate = if suffix == 1 {
                base.clone()
            } else {
                format!("{base}-{suffix}")
            };
            if self
                .store
                .get_job_run_for_workspace(&self.workspace_id, &candidate)?
                .is_none()
            {
                return Ok(candidate);
            }
        }
        Ok(format!("{base}-{job_id}"))
    }
}

impl JobRunStoreBackend for SqliteJobRunStore {
    fn list_job_runs(&self, job_id: &str) -> Result<Vec<JobRun>, OrbitError> {
        validate_path_stem(job_id, "job")?;
        self.list_job_runs_filtered(&JobRunQuery {
            job_id: Some(job_id.to_string()),
            ..Default::default()
        })
    }

    fn list_job_runs_filtered(&self, query: &JobRunQuery) -> Result<Vec<JobRun>, OrbitError> {
        self.store
            .list_job_runs_for_workspace(&self.workspace_id, query)
    }

    fn count_job_runs_filtered(&self, query: &JobRunQuery) -> Result<u64, OrbitError> {
        self.store
            .count_job_runs_for_workspace(&self.workspace_id, query)
    }

    fn list_job_run_durations_filtered(&self, query: &JobRunQuery) -> Result<Vec<u64>, OrbitError> {
        self.store
            .list_job_run_durations_for_workspace(&self.workspace_id, query)
    }

    fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        self.read_run(run_id)
    }

    fn list_pending_or_running_job_runs(&self, job_id: &str) -> Result<Vec<JobRun>, OrbitError> {
        validate_path_stem(job_id, "job")?;
        let mut runs = self.store.list_job_runs_for_workspace(
            &self.workspace_id,
            &JobRunQuery {
                job_id: Some(job_id.to_string()),
                ..Default::default()
            },
        )?;
        runs.retain(|run| matches!(run.state, JobRunState::Pending | JobRunState::Running));
        runs.sort_by_key(|run| std::cmp::Reverse(run.created_at));
        Ok(runs)
    }

    fn insert_job_run(
        &self,
        job_id: &str,
        attempt: u32,
        scheduled_at: DateTime<Utc>,
        input: Option<serde_json::Value>,
        retry_source_run_id: Option<String>,
    ) -> Result<JobRun, OrbitError> {
        validate_path_stem(job_id, "job")?;
        let run = JobRun {
            run_id: self.next_run_id(job_id)?,
            job_id: job_id.to_string(),
            attempt,
            state: JobRunState::Pending,
            scheduled_at,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            created_at: Utc::now(),
            pid: None,
            pid_start_time: None,
            input,
            retry_source_run_id,
            knowledge_metrics: None,
            resolved_crew: None,
            crew_model: None,
            steps: Vec::new(),
        };
        self.store
            .upsert_job_run_for_workspace(&self.workspace_id, &run, None)?;
        Ok(run)
    }

    /// [ORB-11310] The admissions-stop flag and durable child creation share
    /// this SQLite `IMMEDIATE` transaction. SQLite's database writer lock is
    /// process-wide, unlike a Rust mutex: whichever transaction commits first
    /// defines the order seen by every process. The child row, its initial
    /// state, and the parent link commit together, so stop can never
    /// acknowledge between admission and linkage.
    fn admit_child_job_run(
        &self,
        params: &ChildJobRunAdmissionParams,
    ) -> Result<ChildJobRunAdmissionOutcome, OrbitError> {
        validate_path_stem(&params.job_id, "job")?;
        self.store
            .with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
                let parent_row = tx
                    .tx
                    .query_row(
                        "SELECT state, pipeline_state_json FROM job_runs \
                         WHERE workspace_id = ?1 AND run_id = ?2",
                        rusqlite::params![self.workspace_id, params.parent_run_id],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
                    )
                    .map_err(|error| match error {
                        rusqlite::Error::QueryReturnedNoRows => OrbitError::not_found(
                            NotFoundKind::JobRun,
                            params.parent_run_id.clone(),
                        ),
                        other => OrbitError::Store(other.to_string()),
                    })?;
                let parent_run_state = JobRunState::from_str(&parent_row.0).map_err(|error| {
                    OrbitError::Store(format!("invalid job run state: {error}"))
                })?;
                if parent_run_state.is_terminal() {
                    return Err(OrbitError::JobValidation(format!(
                        "parent job run '{}' is {parent_run_state}; a terminal run admits no further work",
                        params.parent_run_id
                    )));
                }
                let raw_parent_state = parent_row.1.ok_or_else(|| {
                    OrbitError::JobValidation(format!(
                        "parent job run '{}' has no pipeline state; child admission cannot be guarded",
                        params.parent_run_id
                    ))
                })?;
                let mut parent_state: PipelineState = serde_json::from_str(&raw_parent_state)
                    .map_err(|error| {
                        OrbitError::Store(format!("invalid pipeline_state_json: {error}"))
                    })?;
                if parent_state.admissions_stopped() {
                    return Ok(ChildJobRunAdmissionOutcome::AdmissionsStopped);
                }

                let run_id = next_run_id_conn(
                    &tx.tx,
                    &self.workspace_id,
                    &params.job_id,
                    params.scheduled_at,
                )?;
                let run = JobRun {
                    run_id: run_id.clone(),
                    job_id: params.job_id.clone(),
                    attempt: params.attempt,
                    state: JobRunState::Pending,
                    scheduled_at: params.scheduled_at,
                    started_at: None,
                    finished_at: None,
                    duration_ms: None,
                    created_at: Utc::now(),
                    pid: None,
                    pid_start_time: None,
                    input: params.input.clone(),
                    retry_source_run_id: None,
                    knowledge_metrics: None,
                    resolved_crew: None,
                    crew_model: None,
                    steps: Vec::new(),
                };
                let child_state = PipelineState::new(
                    run_id.clone(),
                    params.job_id.clone(),
                    params.input.clone().unwrap_or_else(|| serde_json::json!({})),
                );
                parent_state.record_child_dispatch(
                    ChildDispatch::submitted(
                        run_id,
                        params.job_id.clone(),
                        params.action.clone(),
                        params.blocking,
                        false,
                        params.scheduled_at,
                    )
                    .with_parent_step_id(params.parent_step_id.clone()),
                );

                upsert_job_run_for_workspace_conn(
                    &tx.tx,
                    &self.workspace_id,
                    &run,
                    Some(&child_state),
                )?;
                let parent_state_json = serde_json::to_string_pretty(&parent_state)
                    .map_err(|error| OrbitError::Store(format!("serialize pipeline state: {error}")))?;
                tx.tx
                    .execute(
                        "UPDATE job_runs SET pipeline_state_json = ?3 \
                         WHERE workspace_id = ?1 AND run_id = ?2",
                        rusqlite::params![
                            self.workspace_id,
                            params.parent_run_id,
                            parent_state_json
                        ],
                    )
                    .map_err(|error| OrbitError::Store(error.to_string()))?;
                Ok(ChildJobRunAdmissionOutcome::Admitted(Box::new(run)))
            })
    }

    /// [ORB-10965] The single arbiter of job-run start authority.
    ///
    /// Deliberately not routed through [`Self::update_run`]: the decision and
    /// the write must share one immediate transaction, and the duplicate cases
    /// must write nothing at all rather than rewrite identical values.
    fn mark_job_run_running(
        &self,
        run_id: &str,
        started_at: DateTime<Utc>,
        pid: u32,
    ) -> Result<JobRunStartOutcome, OrbitError> {
        let pid_start_time = process_start_identity_token(pid);
        self.store
            .with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
                let Some(mut run) =
                    get_job_run_for_workspace_conn(&tx.tx, &self.workspace_id, run_id)?
                else {
                    return Ok(JobRunStartOutcome::NotFound);
                };

                // A run that already left `pending` was started by someone.
                // Duplicate at-least-once delivery from that same owner is a
                // no-op; anyone else has lost the race to the incumbent.
                if run.state == JobRunState::Running || run.state.is_terminal() {
                    if run.is_owned_by(pid, pid_start_time.as_deref()) {
                        return Ok(JobRunStartOutcome::AlreadyStarted);
                    }
                    return Err(OrbitError::JobRunStartConflict(format!(
                        "run '{}' is already {} under owner pid {} (started at {}); \
                         the start attempt from pid {} has no execution authority",
                        run_id,
                        run.state,
                        run.pid
                            .map_or_else(|| "unknown".to_string(), |owner| owner.to_string()),
                        run.started_at
                            .map_or_else(|| "unknown".to_string(), |at| at.to_rfc3339()),
                        pid,
                    )));
                }

                run.state = run
                    .state
                    .try_transition(RunEvent::Start)
                    .map_err(OrbitError::JobRunStateTransition)?;
                run.started_at = Some(started_at);
                run.pid = Some(pid);
                run.pid_start_time = pid_start_time;
                upsert_job_run_for_workspace_conn(&tx.tx, &self.workspace_id, &run, None)?;
                Ok(JobRunStartOutcome::Started)
            })
    }

    fn claim_pending_job_run_owner(&self, run_id: &str, pid: u32) -> Result<bool, OrbitError> {
        let mut claimed = false;
        let found = self.update_run(run_id, |run| {
            if run.state != JobRunState::Pending {
                return Ok(());
            }
            run.pid = Some(pid);
            run.pid_start_time = process_start_identity_token(pid);
            claimed = true;
            Ok(())
        })?;
        Ok(found && claimed)
    }

    fn complete_job_run_step(
        &self,
        run_id: &str,
        params: &JobRunStepParams,
    ) -> Result<bool, OrbitError> {
        if self.read_run(run_id)?.is_none() {
            return Ok(false);
        }
        params
            .state
            .validate_step_state()
            .map_err(OrbitError::JobRunStateTransition)?;
        let step = JobRunStep {
            step_index: params.step_index as u32,
            target_type: params.target_type,
            target_id: params.target_id.clone(),
            started_at: Some(params.started_at),
            finished_at: Some(params.finished_at),
            duration_ms: params.duration_ms,
            exit_code: params.exit_code,
            agent_response_json: params.agent_response_json.clone(),
            state: params.state,
            error_code: params.error_code.clone(),
            error_message: params.error_message.clone(),
        };
        self.store
            .upsert_job_run_step_for_workspace(&self.workspace_id, run_id, &step)?;
        Ok(true)
    }

    fn record_job_run_knowledge_metrics(
        &self,
        run_id: &str,
        metrics: KnowledgeRunMetrics,
    ) -> Result<bool, OrbitError> {
        self.update_run(run_id, |run| {
            run.knowledge_metrics = Some(metrics);
            Ok(())
        })
    }

    fn record_job_run_crew(&self, run_id: &str, crew: &Crew) -> Result<bool, OrbitError> {
        self.update_run(run_id, |run| {
            run.resolved_crew = Some(crew.name.clone());
            run.crew_model = Some(crew.assignment.model.clone());
            Ok(())
        })
    }

    fn finalize_job_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError> {
        self.update_run(run_id, |run| {
            if run.state.is_terminal() {
                return Ok(());
            }
            let event = match state {
                JobRunState::Success => RunEvent::Complete,
                JobRunState::Failed => RunEvent::Fail,
                JobRunState::Timeout => RunEvent::Timeout,
                JobRunState::Cancelled => RunEvent::Cancel,
                JobRunState::Interrupted => RunEvent::Interrupt,
                other => {
                    return Err(OrbitError::JobRunStateTransition(format!(
                        "cannot finalize to non-terminal state: {other}"
                    )));
                }
            };
            run.state = run
                .state
                .try_transition(event)
                .map_err(OrbitError::JobRunStateTransition)?;
            run.finished_at = Some(finished_at);
            run.duration_ms = duration_ms;
            Ok(())
        })
    }

    fn repair_terminal_job_run_timing(
        &self,
        run_id: &str,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError> {
        let mut changed = false;
        let found = self.update_run(run_id, |run| {
            if !run.state.is_terminal() {
                return Ok(());
            }
            if run.finished_at.is_none() {
                run.finished_at = Some(finished_at);
                changed = true;
            }
            if run.duration_ms.is_none() {
                run.duration_ms = duration_ms;
                changed = true;
            }
            Ok(())
        })?;
        Ok(found && changed)
    }

    fn list_all_pending_or_running_runs(&self) -> Result<Vec<JobRun>, OrbitError> {
        let mut runs = self.store.list_job_runs_for_workspace(
            &self.workspace_id,
            &JobRunQuery {
                ..Default::default()
            },
        )?;
        runs.retain(|run| matches!(run.state, JobRunState::Pending | JobRunState::Running));
        runs.sort_by_key(|run| std::cmp::Reverse(run.created_at));
        Ok(runs)
    }

    fn archive_job_run(&self, run_id: &str) -> Result<String, OrbitError> {
        let run = self
            .read_run(run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
        self.store
            .delete_job_run_for_workspace(&self.workspace_id, run_id)?;
        Ok(run.job_id)
    }

    fn delete_job_run(&self, run_id: &str) -> Result<String, OrbitError> {
        let run = self
            .read_run(run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
        self.store
            .delete_job_run_for_workspace(&self.workspace_id, run_id)?;
        Ok(run.job_id)
    }

    fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError> {
        self.store
            .read_job_run_state_for_workspace(&self.workspace_id, run_id)
    }

    fn write_run_state(&self, run_id: &str, state: &PipelineState) -> Result<(), OrbitError> {
        self.store
            .write_job_run_state_for_workspace(&self.workspace_id, run_id, state)
    }

    fn update_run_state(
        &self,
        run_id: &str,
        update: &mut dyn FnMut(JobRunState, &mut PipelineState) -> Result<(), OrbitError>,
    ) -> Result<RunStateUpdate, OrbitError> {
        self.store
            .update_job_run_state_for_workspace(&self.workspace_id, run_id, update)
    }
}

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

fn upsert_job_run_for_workspace_conn(
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

fn next_run_id_conn(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    job_id: &str,
    submitted_at: DateTime<Utc>,
) -> Result<String, OrbitError> {
    let base = format!("jrun-{}", submitted_at.format("%Y%m%d-%H%M"));
    for suffix in 1..1024_u32 {
        let candidate = if suffix == 1 {
            base.clone()
        } else {
            format!("{base}-{suffix}")
        };
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
    Ok(format!("{base}-{job_id}"))
}

fn get_job_run_for_workspace_conn(
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

/// Run ids per `IN (...)` list, under SQLite's bound-parameter cap.
const STEP_RUN_ID_CHUNK: usize = 500;

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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::Duration;

    use chrono::Utc;
    use orbit_types::workflow::JobTargetType;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn job_run_lifecycle_round_trips() {
        let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
        let scheduled_at = Utc::now();
        let run = backend
            .insert_job_run("job-a", 1, scheduled_at, None, None)
            .expect("insert");
        assert_eq!(run.state, JobRunState::Pending);

        assert!(
            backend
                .mark_job_run_running(&run.run_id, scheduled_at, 42)
                .expect("running")
                .owns_execution()
        );
        let step_params = JobRunStepParams {
            step_index: 0,
            target_type: JobTargetType::Activity,
            target_id: "activity-a".to_string(),
            started_at: scheduled_at,
            finished_at: scheduled_at,
            duration_ms: Some(7),
            exit_code: Some(0),
            agent_response_json: Some(serde_json::json!({"ok": true})),
            state: JobRunState::Success,
            error_code: None,
            error_message: None,
        };
        assert!(
            backend
                .complete_job_run_step(&run.run_id, &step_params)
                .expect("step")
        );
        assert!(
            backend
                .finalize_job_run(&run.run_id, JobRunState::Success, scheduled_at, Some(7))
                .expect("finalize")
        );
        let loaded = backend
            .get_job_run(&run.run_id)
            .expect("get")
            .expect("some");
        assert_eq!(loaded.state, JobRunState::Success);
        assert_eq!(loaded.steps.len(), 1);
    }

    /// [ORB-10070] A pending run accepts an owner claim (pid recorded); once
    /// the run leaves `pending` the claim is refused without writing.
    #[test]
    fn claim_pending_job_run_owner_only_claims_pending_runs() {
        let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
        let scheduled_at = Utc::now();
        let run = backend
            .insert_job_run("job-claim", 1, scheduled_at, None, None)
            .expect("insert");
        assert!(run.pid.is_none());

        assert!(
            backend
                .claim_pending_job_run_owner(&run.run_id, 4242)
                .expect("claim pending")
        );
        let claimed = backend
            .get_job_run(&run.run_id)
            .expect("get")
            .expect("some");
        assert_eq!(claimed.state, JobRunState::Pending);
        assert_eq!(claimed.pid, Some(4242));

        assert!(
            backend
                .mark_job_run_running(&run.run_id, scheduled_at, 4242)
                .expect("running")
                .owns_execution()
        );
        assert!(
            !backend
                .claim_pending_job_run_owner(&run.run_id, 9999)
                .expect("claim running is refused")
        );
        let running = backend
            .get_job_run(&run.run_id)
            .expect("get")
            .expect("some");
        assert_eq!(running.pid, Some(4242));

        assert!(
            !backend
                .claim_pending_job_run_owner("jrun-missing", 4242)
                .expect("claim missing run is refused")
        );
    }

    #[test]
    fn legacy_role_model_row_loads_as_flat_crew_model() {
        let temp = TempDir::new().expect("tempdir");
        let db_path = temp.path().join("orbit.db");
        drop(Store::open(&db_path).expect("create current schema"));

        let conn = rusqlite::Connection::open(&db_path).expect("open raw db");
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO job_runs(
                run_id, workspace_id, job_id, attempt, state, scheduled_at, created_at,
                resolved_crew, implementer_model
             ) VALUES (?1, ?2, ?3, 1, 'success', ?4, ?4, ?5, ?6)",
            rusqlite::params![
                "legacy-run",
                "ws_a",
                "legacy-job",
                now,
                "legacy-crew",
                "legacy-implementer-model"
            ],
        )
        .expect("insert legacy-shaped row");
        drop(conn);

        let loaded = SqliteJobRunStore::new(
            Store::open(&db_path).expect("reopen migrated store"),
            "ws_a",
        )
        .get_job_run("legacy-run")
        .expect("read legacy run")
        .expect("legacy run exists");

        assert_eq!(loaded.resolved_crew.as_deref(), Some("legacy-crew"));
        assert_eq!(
            loaded.crew_model.as_deref(),
            Some("legacy-implementer-model")
        );
    }

    /// [ORB-10002] Checkpoint storage round-trip: per-step recovery metadata
    /// written into `pipeline_state_json` survives reload, and finalizing to
    /// `interrupted` is a valid transition out of `running`.
    #[test]
    fn pipeline_state_checkpoints_round_trip_and_interrupted_finalize() {
        let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
        let scheduled_at = Utc::now();
        let run = backend
            .insert_job_run("job-ckpt", 1, scheduled_at, None, None)
            .expect("insert");
        assert!(
            backend
                .mark_job_run_running(&run.run_id, scheduled_at, 42)
                .expect("running")
                .owns_execution()
        );

        let mut state = PipelineState::new(
            run.run_id.clone(),
            run.job_id.clone(),
            serde_json::json!({"seconds": 0}),
        );
        state.record_step(
            0,
            JobRunState::Success,
            Some(serde_json::json!({"ok": true})),
            None,
        );
        state.sync_pipeline(serde_json::json!({"s0": {"ok": true}}));
        backend
            .write_run_state(&run.run_id, &state)
            .expect("write checkpoint state");

        let loaded = backend
            .read_run_state(&run.run_id)
            .expect("read state")
            .expect("state exists");
        assert_eq!(loaded.step_states.get(&0), Some(&JobRunState::Success));
        assert_eq!(
            loaded.step_outputs.get(&0),
            Some(&serde_json::json!({"ok": true}))
        );
        assert_eq!(loaded.next_step_index, 1);
        assert_eq!(loaded.pipeline, serde_json::json!({"s0": {"ok": true}}));

        assert!(
            backend
                .finalize_job_run(&run.run_id, JobRunState::Interrupted, Utc::now(), Some(1))
                .expect("finalize interrupted")
        );
        let interrupted = backend
            .get_job_run(&run.run_id)
            .expect("get")
            .expect("some");
        assert_eq!(interrupted.state, JobRunState::Interrupted);
        // Checkpoint state survives finalization for a later resume.
        assert!(
            backend
                .read_run_state(&run.run_id)
                .expect("read state after finalize")
                .is_some()
        );
    }

    /// [ORB-10965] At-least-once delivery redelivers a Start to the worker
    /// that already owns the run. That is a no-op, not a state-machine
    /// violation: no timestamp, owner identity, or checkpoint moves.
    #[test]
    fn duplicate_start_from_the_same_owner_is_a_no_op() {
        let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
        let pid = std::process::id();
        let run = backend
            .insert_job_run("job-dup", 1, Utc::now(), None, None)
            .expect("insert");

        let first_started_at = Utc::now();
        assert_eq!(
            backend
                .mark_job_run_running(&run.run_id, first_started_at, pid)
                .expect("first start"),
            JobRunStartOutcome::Started
        );
        let claimed = backend
            .get_job_run(&run.run_id)
            .expect("get")
            .expect("some");

        let mut state = PipelineState::new(
            run.run_id.clone(),
            run.job_id.clone(),
            serde_json::json!({"seconds": 0}),
        );
        state.record_step(0, JobRunState::Success, None, None);
        backend
            .write_run_state(&run.run_id, &state)
            .expect("write checkpoint");

        // The redelivery carries a later timestamp; the incumbent's stays.
        let outcome = backend
            .mark_job_run_running(&run.run_id, first_started_at + Duration::from_secs(30), pid)
            .expect("redelivered start succeeds");
        assert_eq!(outcome, JobRunStartOutcome::AlreadyStarted);
        assert!(
            !outcome.owns_execution(),
            "a redelivery must not grant a second execution"
        );

        let after = backend
            .get_job_run(&run.run_id)
            .expect("get")
            .expect("some");
        assert_eq!(after.state, JobRunState::Running);
        assert_eq!(after.started_at, claimed.started_at);
        assert_eq!(after.pid, Some(pid));
        assert_eq!(after.pid_start_time, claimed.pid_start_time);
        assert_eq!(
            backend
                .read_run_state(&run.run_id)
                .expect("read checkpoint")
                .expect("checkpoint survives")
                .step_states
                .get(&0),
            Some(&JobRunState::Success),
            "checkpoints must survive a deduplicated start"
        );
    }

    /// [ORB-10965] A duplicate Start whose owner differs from the incumbent's
    /// is reported as a conflict naming both, not as a generic transition
    /// error, so the loser can yield instead of failing the run.
    #[test]
    fn duplicate_start_from_a_different_owner_is_a_specific_conflict() {
        let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
        let incumbent = std::process::id();
        let run = backend
            .insert_job_run("job-conflict", 1, Utc::now(), None, None)
            .expect("insert");
        let started_at = Utc::now();
        backend
            .mark_job_run_running(&run.run_id, started_at, incumbent)
            .expect("incumbent start");

        let challenger = incumbent + 1;
        let error = backend
            .mark_job_run_running(&run.run_id, Utc::now(), challenger)
            .expect_err("a competing owner has no authority");
        let message = match &error {
            OrbitError::JobRunStartConflict(message) => message.clone(),
            other => panic!("expected a start conflict, got {other:?}"),
        };
        assert!(message.contains(&incumbent.to_string()), "{message}");
        assert!(message.contains(&challenger.to_string()), "{message}");

        let after = backend
            .get_job_run(&run.run_id)
            .expect("get")
            .expect("some");
        assert_eq!(after.state, JobRunState::Running);
        assert_eq!(after.pid, Some(incumbent));

        // The same rule holds once the run has finished: a late duplicate
        // cannot reopen a terminal run, and the refusal still names the race.
        backend
            .finalize_job_run(&run.run_id, JobRunState::Success, Utc::now(), Some(5))
            .expect("finalize");
        assert!(matches!(
            backend.mark_job_run_running(&run.run_id, Utc::now(), challenger),
            Err(OrbitError::JobRunStartConflict(_))
        ));
        assert_eq!(
            backend
                .mark_job_run_running(&run.run_id, Utc::now(), incumbent)
                .expect("owner redelivery after completion"),
            JobRunStartOutcome::AlreadyStarted
        );
        assert_eq!(
            backend
                .get_job_run(&run.run_id)
                .expect("get")
                .expect("some")
                .state,
            JobRunState::Success,
            "a duplicate start must not resurrect a finished run"
        );
    }

    /// [ORB-10965] Deduplication is scoped to states a Start can legitimately
    /// race with. A Start from `retrying` is still a genuine state-machine
    /// violation and keeps rejecting.
    #[test]
    fn start_from_an_illegal_state_still_rejects_as_a_transition_error() {
        let backend = SqliteJobRunStore::new(Store::open_in_memory().expect("store"), "ws_a");
        let run = backend
            .insert_job_run("job-illegal", 1, Utc::now(), None, None)
            .expect("insert");
        let mut retrying = run.clone();
        retrying.state = JobRunState::Retrying;
        backend
            .store
            .upsert_job_run_for_workspace("ws_a", &retrying, None)
            .expect("force retrying state");

        assert!(matches!(
            backend.mark_job_run_running(&run.run_id, Utc::now(), std::process::id()),
            Err(OrbitError::JobRunStateTransition(_))
        ));
    }

    /// [ORB-10965] The duplicate-delivery race itself: two workers hand the
    /// same queued run to the store at the same instant. The immediate
    /// transaction serializes them, so exactly one is granted execution.
    #[test]
    fn competing_starts_grant_execution_authority_to_exactly_one_worker() {
        let temp = TempDir::new().expect("tempdir");
        let db_path = temp.path().join("orbit.db");
        let backend_a = SqliteJobRunStore::new(Store::open(&db_path).expect("store a"), "ws_a");
        let backend_b = SqliteJobRunStore::new(Store::open(&db_path).expect("store b"), "ws_a");
        let run = backend_a
            .insert_job_run("job-race", 1, Utc::now(), None, None)
            .expect("insert");
        let run_id = run.run_id.clone();
        let barrier = Arc::new(Barrier::new(2));

        let mut workers = Vec::new();
        for (backend, pid) in [(backend_a, 100_001_u32), (backend_b, 100_002_u32)] {
            let run_id = run_id.clone();
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                (pid, backend.mark_job_run_running(&run_id, Utc::now(), pid))
            }));
        }
        let results: Vec<(u32, Result<JobRunStartOutcome, OrbitError>)> = workers
            .into_iter()
            .map(|worker| worker.join().expect("worker"))
            .collect();

        let winners: Vec<u32> = results
            .iter()
            .filter(|(_, result)| matches!(result, Ok(outcome) if outcome.owns_execution()))
            .map(|(pid, _)| *pid)
            .collect();
        assert_eq!(
            winners.len(),
            1,
            "exactly one worker may advance: {results:?}"
        );
        assert!(
            results.iter().any(|(pid, result)| *pid != winners[0]
                && matches!(result, Err(OrbitError::JobRunStartConflict(_)))),
            "the loser must yield with a start conflict: {results:?}"
        );

        let stored = SqliteJobRunStore::new(Store::open(&db_path).expect("store c"), "ws_a")
            .get_job_run(&run_id)
            .expect("read")
            .expect("run");
        assert_eq!(stored.state, JobRunState::Running);
        assert_eq!(stored.pid, Some(winners[0]));
    }

    #[test]
    fn update_run_serializes_concurrent_mutations_without_torn_write() {
        let temp = TempDir::new().expect("tempdir");
        let db_path = temp.path().join("orbit.db");
        let backend_a = SqliteJobRunStore::new(Store::open(&db_path).expect("store a"), "ws_a");
        let backend_b = SqliteJobRunStore::new(Store::open(&db_path).expect("store b"), "ws_a");
        let scheduled_at = Utc::now();
        let run = backend_a
            .insert_job_run("job-a", 1, scheduled_at, None, None)
            .expect("insert");
        let run_id = run.run_id.clone();
        let barrier = Arc::new(Barrier::new(2));

        let run_id_a = run_id.clone();
        let barrier_a = Arc::clone(&barrier);
        let writer_a = thread::spawn(move || {
            backend_a.update_run(&run_id_a, |run| {
                run.resolved_crew = Some("crew-a".to_string());
                barrier_a.wait();
                thread::sleep(Duration::from_millis(100));
                Ok(())
            })
        });

        barrier.wait();
        let run_id_b = run_id.clone();
        let writer_b = thread::spawn(move || {
            backend_b.update_run(&run_id_b, |run| {
                run.knowledge_metrics = Some(KnowledgeRunMetrics {
                    raw_read_token_baseline: 100,
                    knowledge_pack_tokens: Some(50),
                    compression_ratio: Some(2.0),
                    actual_fs_read_tokens_during_run: 25,
                    double_read_rate: Some(0.0),
                    knowledge_pack_used: true,
                    knowledge_pack_unresolved_count: 0,
                    total_llm_input_tokens: 75,
                });
                Ok(())
            })
        });

        assert!(writer_a.join().expect("writer a").expect("update a"));
        assert!(writer_b.join().expect("writer b").expect("update b"));

        let loaded = SqliteJobRunStore::new(Store::open(&db_path).expect("store c"), "ws_a")
            .get_job_run(&run_id)
            .expect("read")
            .expect("run");
        assert_eq!(loaded.resolved_crew.as_deref(), Some("crew-a"));
        assert!(loaded.knowledge_metrics.is_some());
    }
}
