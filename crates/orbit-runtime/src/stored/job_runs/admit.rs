//! Run insertion: top-level, resume, keyed, automation and child admissions.
//!
//! Id allocation and the insert share one `Store::write` transaction
//! [ORB-12111]: two top-level submissions in the same minute compete for the
//! same sequence, and a candidate probed outside the write would let the loser
//! upsert over the sibling that committed first. Each admission's probe (live
//! lineage, retry key, automation key, parent admissions stop) commits with
//! its insert, so concurrent submitters from any process admit exactly one.

use chrono::{DateTime, Utc};
use orbit_common::process::build::current_orbit_build;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::contracts::{
    ChildJobRunAdmissionOutcome, ChildJobRunAdmissionParams, KeyedJobRunAdmission,
    KeyedJobRunParams,
};
use orbit_types::workflow::{ChildDispatch, JobRun, JobRunState, PipelineState, RunIdRole};
use serde_json::Value;

use super::{JobRuns, validate_job_id};

/// One pending run's fields; everything else starts empty.
struct NewRun {
    run_id: String,
    job_id: String,
    attempt: u32,
    scheduled_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    input: Option<Value>,
    retry_source_run_id: Option<String>,
}

impl JobRuns {
    fn pending_run(&self, new: NewRun) -> JobRun {
        JobRun {
            build: Some(current_orbit_build()),
            executed_on: self.executed_on.clone(),
            run_id: new.run_id,
            job_id: new.job_id,
            attempt: new.attempt,
            state: JobRunState::Pending,
            scheduled_at: new.scheduled_at,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            created_at: new.created_at,
            pid: None,
            pid_start_time: None,
            input: new.input,
            retry_source_run_id: new.retry_source_run_id,
            knowledge_metrics: None,
            resolved_crew: None,
            crew_model: None,
            steps: Vec::new(),
        }
    }

    pub fn insert_job_run(
        &self,
        job_id: &str,
        attempt: u32,
        scheduled_at: DateTime<Utc>,
        input: Option<Value>,
        retry_source_run_id: Option<String>,
    ) -> Result<JobRun, OrbitError> {
        validate_job_id(job_id)?;
        let created_at = Utc::now();
        self.store.write(|tx| {
            let runs = tx.job_run();
            let run = self.pending_run(NewRun {
                run_id: runs.next_id(RunIdRole::TopLevel, created_at)?,
                job_id: job_id.to_string(),
                attempt,
                scheduled_at,
                created_at,
                input,
                retry_source_run_id,
            });
            runs.put(&self.workspace_id, &run, None)?;
            Ok(run)
        })
    }

    /// Admit a resume of `retry_source_run_id` unless its retry lineage
    /// already has a live run.
    ///
    /// The lineage is the source's `retry_source_run_id` ancestors and every
    /// run descended from any of them: all of them reuse the same
    /// checkpointed worktree and task claims. When one is `pending`,
    /// `running` or `retrying`, this inserts nothing and fails with
    /// [`OrbitError::ResumeRunInFlight`] naming the oldest such run.
    pub fn insert_resume_job_run(
        &self,
        job_id: &str,
        attempt: u32,
        scheduled_at: DateTime<Utc>,
        input: Option<Value>,
        retry_source_run_id: &str,
    ) -> Result<JobRun, OrbitError> {
        validate_job_id(job_id)?;
        let created_at = Utc::now();
        self.store.write(|tx| {
            let runs = tx.job_run();
            if let Some(run_id) =
                runs.get_live_in_lineage(&self.workspace_id, retry_source_run_id)?
            {
                return Err(OrbitError::ResumeRunInFlight {
                    source_run_id: retry_source_run_id.to_string(),
                    run_id,
                });
            }
            let run = self.pending_run(NewRun {
                run_id: runs.next_id(RunIdRole::TopLevel, created_at)?,
                job_id: job_id.to_string(),
                attempt,
                scheduled_at,
                created_at,
                input,
                retry_source_run_id: Some(retry_source_run_id.to_string()),
            });
            runs.put(&self.workspace_id, &run, None)?;
            Ok(run)
        })
    }

    /// Resolve or admit a run submitted under a caller retry key.
    ///
    /// The key is the string `params.input[params.retry_key_field]`. Within
    /// the newest `params.scan_limit` runs of `params.job_id` (newest first),
    /// the first whose input carries the same key is returned as
    /// [`KeyedJobRunAdmission::Existing`] and nothing is written. Otherwise a
    /// fresh pending run is inserted and returned as
    /// [`KeyedJobRunAdmission::Admitted`]. A key older than the window is not
    /// recognized: the key is a retry handle, not a permanent constraint.
    pub fn insert_keyed_job_run(
        &self,
        params: &KeyedJobRunParams,
    ) -> Result<KeyedJobRunAdmission, OrbitError> {
        validate_job_id(&params.job_id)?;
        let key = params
            .input
            .get(&params.retry_key_field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "keyed job run input must carry a non-blank `{}`",
                    params.retry_key_field
                ))
            })?;
        let created_at = Utc::now();
        self.store.write(|tx| {
            let runs = tx.job_run();
            // The window uses the run list's default `created_at DESC,
            // run_id ASC` order. An unreadable input cannot carry the key; it
            // must not wedge every later submission of the job.
            let keyed = runs
                .list_keyed_window(&self.workspace_id, &params.job_id, params.scan_limit)?
                .into_iter()
                .find(|(_, input_json)| {
                    input_json
                        .as_deref()
                        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                        .is_some_and(|input| {
                            input.get(&params.retry_key_field).and_then(Value::as_str) == Some(key)
                        })
                });
            if let Some((run_id, _)) = keyed {
                let run = runs
                    .get(&self.workspace_id, &run_id)?
                    .ok_or_else(|| OrbitError::Store("keyed run disappeared".into()))?;
                return Ok(KeyedJobRunAdmission::Existing(Box::new(run)));
            }
            let run = self.pending_run(NewRun {
                run_id: runs.next_id(RunIdRole::TopLevel, created_at)?,
                job_id: params.job_id.clone(),
                attempt: 1,
                scheduled_at: params.scheduled_at,
                created_at,
                input: Some(params.input.clone()),
                retry_source_run_id: None,
            });
            runs.put(&self.workspace_id, &run, None)?;
            Ok(KeyedJobRunAdmission::Admitted(Box::new(run)))
        })
    }

    /// Insert a run for a durable automation action key, or resolve the run
    /// already bound to it without writing that run. An existing key must
    /// name the same job and input. The outcome is decided inside the
    /// insertion transaction so callers can initialize only newly admitted
    /// runs.
    pub fn insert_automation_job_run(
        &self,
        job_id: &str,
        input: Value,
        key: &str,
    ) -> Result<KeyedJobRunAdmission, OrbitError> {
        validate_job_id(job_id)?;
        self.store.write(|tx| {
            let runs = tx.job_run();
            if let Some(id) = runs.get_automation_key(&self.workspace_id, key)? {
                let run = runs
                    .get(&self.workspace_id, &id)?
                    .ok_or_else(|| OrbitError::Store("automation run missing".into()))?;
                if run.job_id != job_id || run.input.as_ref() != Some(&input) {
                    return Err(OrbitError::InvalidInput(
                        "automation job key input changed".into(),
                    ));
                }
                return Ok(KeyedJobRunAdmission::Existing(Box::new(run)));
            }
            let now = Utc::now();
            let id = runs.next_id(RunIdRole::TopLevel, now)?;
            let run = self.pending_run(NewRun {
                run_id: id.clone(),
                job_id: job_id.into(),
                attempt: 1,
                scheduled_at: now,
                created_at: now,
                input: Some(input.clone()),
                retry_source_run_id: None,
            });
            let state = PipelineState::new(id.clone(), job_id.into(), input);
            runs.put(&self.workspace_id, &run, Some(&state))?;
            runs.put_automation_key(&self.workspace_id, key, &id)?;
            Ok(KeyedJobRunAdmission::Admitted(Box::new(run)))
        })
    }

    /// [ORB-11310] Admit and link a child run unless its parent has stopped
    /// admissions.
    ///
    /// The parent-state read, the child insert with its initial state and the
    /// parent's dispatch checkpoint commit in one transaction. That commit is
    /// the cross-process linearization point shared with an admissions-stop
    /// update: a stop that commits first makes this return
    /// [`ChildJobRunAdmissionOutcome::AdmissionsStopped`], while a child that
    /// commits first is already linked when stop acknowledges.
    pub fn admit_child_job_run(
        &self,
        params: &ChildJobRunAdmissionParams,
    ) -> Result<ChildJobRunAdmissionOutcome, OrbitError> {
        validate_job_id(&params.job_id)?;
        self.store.write(|tx| {
            let runs = tx.job_run();
            let (parent_run_state, raw_parent_state) = runs
                .get_with_pipeline_state(&self.workspace_id, &params.parent_run_id)?
                .ok_or_else(|| {
                    OrbitError::not_found(NotFoundKind::JobRun, params.parent_run_id.clone())
                })?;
            if parent_run_state.is_terminal() {
                return Err(OrbitError::JobValidation(format!(
                    "parent job run '{}' is {parent_run_state}; a terminal run admits no further work",
                    params.parent_run_id
                )));
            }
            let raw_parent_state = raw_parent_state.ok_or_else(|| {
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

            let run_id = runs.next_id(RunIdRole::Child, params.scheduled_at)?;
            let run = self.pending_run(NewRun {
                run_id: run_id.clone(),
                job_id: params.job_id.clone(),
                attempt: params.attempt,
                scheduled_at: params.scheduled_at,
                created_at: Utc::now(),
                input: params.input.clone(),
                retry_source_run_id: None,
            });
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

            runs.put(&self.workspace_id, &run, Some(&child_state))?;
            let parent_state_json = serde_json::to_string(&parent_state)
                .map_err(|error| OrbitError::Store(format!("serialize pipeline state: {error}")))?;
            runs.put_pipeline_state_json(
                &self.workspace_id,
                &params.parent_run_id,
                &parent_state_json,
            )?;
            Ok(ChildJobRunAdmissionOutcome::Admitted(Box::new(run)))
        })
    }
}
