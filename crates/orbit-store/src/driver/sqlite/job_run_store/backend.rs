//! `SqliteJobRunStore` and the `JobRunStoreBackend` implementation.

use std::collections::HashMap;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use orbit_common::process::identity::process_start_identity_token;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::identity::Crew;
use orbit_types::workflow::{
    ChildDispatch, JobRun, JobRunStartOutcome, JobRunState, JobRunStep, KnowledgeRunMetrics,
    PipelineState, RunEvent, RunStateUpdate,
};
use rusqlite::{OptionalExtension, TransactionBehavior};

use super::queries::{
    get_job_run_for_workspace_conn, next_run_id_conn, upsert_job_run_for_workspace_conn,
};
use crate::Store;
use crate::contracts::{
    ChildJobRunAdmissionOutcome, ChildJobRunAdmissionParams, JobRunQuery, JobRunStepParams,
    JobRunStoreBackend,
};
use crate::fs::path_safety::validate_path_stem;

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

    /// Read-modify-write a run row inside one immediate transaction.
    ///
    /// `pub(crate)` so sibling tests can inject a barrier into the mutation
    /// closure and prove concurrent writers serialize without a torn write.
    pub(crate) fn update_run(
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
    fn job_run_retries(&self, run_id: &str, limit: usize) -> Result<Vec<JobRun>, OrbitError> {
        self.store.with_read_connection(|conn| {
            let mut statement = conn.prepare("SELECT run_id FROM job_runs WHERE workspace_id=?1 AND retry_source_run_id=?2 ORDER BY created_at,run_id LIMIT ?3")
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            let ids = statement.query_map(rusqlite::params![self.workspace_id,run_id,limit.min(1000)], |row|row.get::<_,String>(0))
                .map_err(|error| OrbitError::Store(error.to_string()))?
                .collect::<Result<Vec<_>,_>>().map_err(|error| OrbitError::Store(error.to_string()))?;
            ids.into_iter().map(|id| get_job_run_for_workspace_conn(conn, &self.workspace_id, &id)?
                .ok_or_else(|| OrbitError::Store("retry run disappeared".into()))).collect()
        })
    }

    fn automation_job_for_key(&self, key: &str) -> Result<Option<String>, OrbitError> {
        self.store.with_read_connection(|conn| {
            conn.query_row(
                "SELECT run_id FROM automation_job_keys WHERE workspace_id=?1 AND action_key=?2",
                rusqlite::params![self.workspace_id, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| OrbitError::Store(error.to_string()))
        })
    }

    fn insert_automation_job_run(
        &self,
        job_id: &str,
        input: serde_json::Value,
        key: &str,
    ) -> Result<JobRun, OrbitError> {
        validate_path_stem(job_id, "job")?;
        super::super::automation::initialize(&self.store)?;
        self.store.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let conn=tx.connection();
            let existing:Option<String>=conn.query_row("SELECT run_id FROM automation_job_keys WHERE workspace_id=?1 AND action_key=?2",rusqlite::params![self.workspace_id,key],|r|r.get(0)).optional().map_err(|e|OrbitError::Store(e.to_string()))?;
            if let Some(id)=existing {
                let run=get_job_run_for_workspace_conn(conn,&self.workspace_id,&id)?.ok_or_else(|| OrbitError::Store("automation run missing".into()))?;
                if run.job_id!=job_id || run.input.as_ref()!=Some(&input) {return Err(OrbitError::InvalidInput("automation job key input changed".into()));}
                return Ok(run);
            }
            let now=Utc::now();
            let id=next_run_id_conn(conn,&self.workspace_id,job_id,now)?;
            let run=JobRun {run_id:id.clone(),job_id:job_id.into(),attempt:1,state:JobRunState::Pending,scheduled_at:now,started_at:None,finished_at:None,duration_ms:None,created_at:now,pid:None,pid_start_time:None,input:Some(input.clone()),retry_source_run_id:None,knowledge_metrics:None,resolved_crew:None,crew_model:None,steps:Vec::new()};
            let state=PipelineState::new(id.clone(),job_id.into(),input.clone());
            upsert_job_run_for_workspace_conn(conn,&self.workspace_id,&run,Some(&state))?;
            conn.execute("INSERT INTO automation_job_keys VALUES (?1,?2,?3)",rusqlite::params![self.workspace_id,key,id]).map_err(|e|OrbitError::Store(e.to_string()))?;
            Ok(run)
        })
    }

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
        self.store.list_job_runs_for_workspace(
            &self.workspace_id,
            &JobRunQuery {
                job_id: Some(job_id.to_string()),
                active_only: true,
                ..Default::default()
            },
        )
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
        if params.authority.is_some() {
            super::super::operation::initialize(&self.store)?;
        }
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
                // [ORB-11332] A grant-bound parent rechecks its grant here, in
                // the same transaction, so a stop, expiry, or revocation that
                // committed first is seen before the child exists.
                if let Some(authority) = &params.authority
                    && let Some(reason) = super::super::operation::admission_refusal(
                        &tx.tx,
                        &self.workspace_id,
                        &params.job_id,
                        authority,
                    )?
                {
                    return Ok(ChildJobRunAdmissionOutcome::Refused { reason });
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
        super::start::mark_job_run_running(&self.store, &self.workspace_id, run_id, started_at, pid)
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
        self.store.list_job_runs_for_workspace(
            &self.workspace_id,
            &JobRunQuery {
                active_only: true,
                ..Default::default()
            },
        )
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

    fn read_run_states(
        &self,
        run_ids: &[String],
    ) -> Result<HashMap<String, Option<PipelineState>>, OrbitError> {
        self.store
            .read_job_run_states_for_workspace(&self.workspace_id, run_ids)
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
