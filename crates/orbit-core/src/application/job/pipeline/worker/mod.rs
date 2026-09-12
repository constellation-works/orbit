use super::*;
use command::*;
use log::*;

use super::admission::pipeline_run_is_runnable;
use super::wait::PIPELINE_WAIT_MIN_POLL_SECONDS;

pub(super) mod command;
pub(super) mod log;

impl OrbitRuntime {
    pub fn execute_pipeline_run_worker(&self, run_id: &str) -> Result<(), OrbitError> {
        self.preflight_pipeline_worker_store()?;

        // [ORB-10070] Claim the queued run for this worker process so orphan
        // reconciliation can tell a pending run whose worker is alive and
        // polling for its admission slot apart from one whose worker died
        // (crash, SIGKILL, host reboot). Best-effort: the run may already be
        // running/terminal, and a claim failure must never block execution.
        if let Err(error) = self
            .stores()
            .jobs()
            .claim_pending_job_run_owner(run_id, std::process::id())
        {
            tracing::warn!(
                target: "orbit.core.job_run",
                run_id,
                error = %error,
                "pipeline worker could not claim its pending run; orphan \
                 detection falls back to the unclaimed-run grace window",
            );
        }
        loop {
            let run = self.show_job_run(run_id)?;
            match run.state {
                JobRunState::Pending => {}
                JobRunState::Running
                | JobRunState::Success
                | JobRunState::Failed
                | JobRunState::Timeout
                | JobRunState::Cancelled
                | JobRunState::Interrupted => return Ok(()),
                other => {
                    return Err(OrbitError::Execution(format!(
                        "pipeline worker cannot execute run '{}' from state '{}'",
                        run_id, other
                    )));
                }
            }

            let (yaml_path, spec) = self.resolve_run_definition(&run)?;
            if spec.state != JobScheduleState::Enabled {
                let _ = self.cancel_job_run(&run.run_id);
                return Err(OrbitError::InvalidInput(format!(
                    "job '{}' is disabled",
                    run.job_id
                )));
            }

            if let Err(error) = self.verify_routine_dispatch_workspace(&run) {
                self.record_routine_dispatch_workspace_mismatch(&run, &error);
                let _ = self.cancel_job_run(&run.run_id);
                return Err(error);
            }

            self.reconcile_stale_job_runs(Some(&run.job_id))?;
            let active_runs = self
                .stores()
                .jobs()
                .list_pending_or_running_job_runs(&run.job_id)?;
            if !pipeline_run_is_runnable(&active_runs, &run.run_id, spec.max_active_runs) {
                thread::sleep(Duration::from_secs(PIPELINE_WAIT_MIN_POLL_SECONDS));
                continue;
            }

            return self.execute_pipeline_run_now(&run, &yaml_path);
        }
    }
    /// [ORB-11998] A routine-dispatched run declares the `.orbit` directory of
    /// the workspace that owns it (see [`ROUTINE_DISPATCH_ORBIT_DIR_FIELD`]).
    /// Confirm this worker actually opened that same workspace before it runs
    /// any step. A mismatch — an ambient `ORBIT_ROOT`, an unregistered cwd, or
    /// any other workspace-routing failure — must fail the run visibly rather
    /// than silently execute (or vacuously succeed) against the wrong scope.
    /// A run with no declared field is not routine-dispatched and is
    /// unaffected.
    ///
    /// [ORB-12038] The refusal terminalizes the run as `cancelled`, not
    /// `failed`. It reuses [`Self::cancel_job_run`] unchanged — the same
    /// request/signal/completion audit trail, reservation release, and
    /// child-cascade settlement that every other cancellation gets — because
    /// nothing about that machinery is wrong here; only the missing
    /// diagnostic was. `failed` would read more accurately for a routing
    /// refusal than an operator action, but that relabeling is a wider
    /// contract change than this diagnostics fix and is deliberately left
    /// alone; see [`Self::record_routine_dispatch_workspace_mismatch`] for the
    /// diagnostic itself.
    fn verify_routine_dispatch_workspace(&self, run: &JobRun) -> Result<(), OrbitError> {
        let Some(declared) = run
            .input
            .as_ref()
            .and_then(|input| input.get(ROUTINE_DISPATCH_ORBIT_DIR_FIELD))
            .and_then(Value::as_str)
        else {
            return Ok(());
        };
        let declared_dir = Path::new(declared);
        let actual_dir = &self.paths().orbit_dir;
        if declared_dir == actual_dir.as_path() {
            return Ok(());
        }
        Err(OrbitError::WorkspaceError(format!(
            "run '{}' was dispatched for workspace '{}' but this worker resolved workspace '{}'; \
             refusing to execute against a mismatched workspace context",
            run.run_id,
            declared_dir.display(),
            actual_dir.display(),
        )))
    }
    /// [ORB-12038] Persist the guard's own message as a diagnostic step before
    /// [`Self::cancel_job_run`] terminalizes the run.
    ///
    /// The run is still `pending` here (`execute_pipeline_run_worker` has not
    /// reached `execute_pipeline_run_now`, so there is no `running` step to
    /// attach an error to), and cancellation itself records no error detail —
    /// it is written for an operator-requested stop, which carries no
    /// message. Without this, the guard's declared-vs-resolved diagnostic
    /// existed only in the worker process's own stderr and its
    /// `<run_id>.worker.log`, never on the run `orbit run show` displays.
    /// Best-effort: a failure to persist the diagnostic must not stop the
    /// run from being cancelled or the original error from propagating.
    fn record_routine_dispatch_workspace_mismatch(&self, run: &JobRun, error: &OrbitError) {
        let now = Utc::now();
        let _ = self.record_pipeline_diagnostic_step(
            run,
            run.scheduled_at,
            now,
            Some(ROUTINE_DISPATCH_WORKSPACE_MISMATCH_ERROR_CODE),
            &error.to_string(),
            JobRunState::Cancelled,
        );
    }
    /// Reopen the shared SQLite store before the worker claims a run.
    ///
    /// ADR-0287: another Orbit process may advance the host-global database
    /// while this runtime remains alive. Reopening here applies migrations
    /// supported by this binary or trips the downgrade guard before any agent
    /// work. Invocation persistence still reopens independently, but it can no
    /// longer be the first compatibility check after useful work completes.
    pub(crate) fn preflight_pipeline_worker_store(&self) -> Result<(), OrbitError> {
        self.ensure_persistence_ready()?;
        Ok(())
    }
    fn execute_pipeline_run_now(&self, run: &JobRun, yaml_path: &Path) -> Result<(), OrbitError> {
        let started_at = Utc::now();
        // [ORB-10965] The state read in `execute_pipeline_run_worker` and this
        // Start are separate transactions, so a second worker handed the same
        // queued run can arrive here having also seen `pending`. The store
        // arbitrates atomically; whoever does not win execution authority
        // yields here, before any agent work.
        let outcome = match self.stores().jobs().mark_job_run_running(
            &run.run_id,
            started_at,
            std::process::id(),
        ) {
            Ok(outcome) => outcome,
            Err(OrbitError::JobRunStartConflict(diagnostic)) => {
                return self.record_deduplicated_start(run, &diagnostic);
            }
            Err(error) => return Err(error),
        };
        match outcome {
            JobRunStartOutcome::Started => {}
            JobRunStartOutcome::AlreadyStarted => {
                return self.record_deduplicated_start(
                    run,
                    "this worker process had already started the run",
                );
            }
            JobRunStartOutcome::NotFound => return Ok(()),
        }
        let input = run
            .input
            .clone()
            .unwrap_or_else(|| Value::Object(Default::default()));
        // Once Start succeeds, every later error belongs to this run. Keep the
        // whole setup path inside the outcome finalized below so crew
        // validation, event persistence, and resume-state reads cannot escape
        // with a durable `running` projection.
        let outcome = (|| {
            self.record_run_crew_from_input(&run.run_id, &input)?;

            self.record_event(OrbitEvent::JobRunStarted {
                job_id: run.job_id.clone(),
                run_id: run.run_id.clone(),
                attempt: run.attempt,
            })?;

            // [ORB-10470] The run's own persisted checkpoints are the resume
            // cursor. A run seeded by `submit_resume_run` starts at the
            // source's first non-successful step; a run whose previous worker
            // died after checkpointing continues from where that worker
            // stopped. Reusing a checkpoint is therefore idempotent — the
            // successful steps are skipped, never re-dispatched.
            let resume = self.read_run_state(&run.run_id)?.filter(|state| {
                state
                    .step_states
                    .values()
                    .any(|step_state| *step_state == JobRunState::Success)
            });
            self.run_job_v2_from_yaml_with_run_id_and_resume(
                yaml_path,
                input.clone(),
                Some(run.run_id.clone()),
                run.retry_source_run_id.clone(),
                resume.as_ref(),
            )
        })();
        let finished_at = Utc::now();
        self.finalize_v2_pipeline_run(
            run,
            &input,
            started_at,
            finished_at,
            outcome.as_ref(),
            V2RunFinalizationOptions::DETACHED_WORKER,
        )?;
        outcome.map(|_| ())
    }
    pub(crate) fn record_pipeline_failure_step(
        &self,
        run: &JobRun,
        started_at: chrono::DateTime<Utc>,
        finished_at: chrono::DateTime<Utc>,
        message: &str,
    ) -> Result<(), OrbitError> {
        self.record_pipeline_diagnostic_step(
            run,
            started_at,
            finished_at,
            None,
            message,
            JobRunState::Failed,
        )
    }
    /// [ORB-10002] Record a terminal diagnostic step with an explicit state
    /// (`failed` for job errors, `interrupted` for orphan reconciliation).
    pub(crate) fn record_pipeline_diagnostic_step(
        &self,
        run: &JobRun,
        started_at: chrono::DateTime<Utc>,
        finished_at: chrono::DateTime<Utc>,
        error_code: Option<&str>,
        message: &str,
        state: JobRunState,
    ) -> Result<(), OrbitError> {
        let current = self
            .get_job_run_backend(&run.run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run.run_id.clone()))?;
        let already_has_error = current
            .steps
            .iter()
            .any(|step| step.error_code.is_some() || step.error_message.is_some());
        if already_has_error {
            return Ok(());
        }

        let step_index = current
            .steps
            .iter()
            .map(|step| step.step_index)
            .max()
            .map(|index| index.saturating_add(1) as usize)
            .unwrap_or(0);
        let duration_ms = Some(
            finished_at
                .signed_duration_since(started_at)
                .num_milliseconds()
                .max(0) as u64,
        );
        let params = JobRunStepParams {
            step_index,
            target_type: JobTargetType::Job,
            target_id: run.job_id.clone(),
            started_at,
            finished_at,
            duration_ms,
            exit_code: None,
            agent_response_json: None,
            state,
            error_code: error_code.map(str::to_string),
            error_message: Some(message.to_string()),
        };
        let _ = self
            .stores()
            .jobs()
            .complete_job_run_step(&run.run_id, &params)?;
        Ok(())
    }
    /// [ORB-12038] Read a run's own `<run_id>.worker.log`, for a caller (`orbit
    /// run logs`) that found no audited CLI-invocation blobs to show. A run
    /// that fails before any step runs — a routine-dispatch workspace
    /// mismatch, a worker that could not start at all — has no step-scoped
    /// output to audit; the worker log is where that process wrote its own
    /// stderr, and it is the only place the cause exists.
    ///
    /// `Ok(None)` means no such file exists (the ordinary case for a run that
    /// reached step execution). `Some` with `content: None` means the file
    /// exists but its content could not be recovered (unreadable or empty);
    /// the caller still has the path to name where to look by hand.
    pub fn read_pipeline_worker_log(
        &self,
        run_id: &str,
    ) -> Result<Option<PipelineWorkerLogSnapshot>, OrbitError> {
        let path = pipeline_worker_log_path(&self.paths().logs_dir, run_id)?;
        if !path.is_file() {
            return Ok(None);
        }
        let mut file = File::open(&path).map_err(|error| {
            OrbitError::Io(format!(
                "open pipeline worker log '{}': {error}",
                path.display()
            ))
        })?;
        let content = read_pipeline_worker_log_tail(&mut file);
        Ok(Some(PipelineWorkerLogSnapshot { path, content }))
    }
    pub(super) fn spawn_pipeline_worker(
        &self,
        run_id: &str,
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        let mut command = self.pipeline_worker_command(run_id)?;
        let worker_log =
            configure_pipeline_worker_stdio(&mut command, &self.paths().logs_dir, run_id)?;
        self.spawn_pipeline_worker_process(run_id, actor, command, worker_log)
            .map(|_| ())
    }
    pub(crate) fn monitor_pipeline_worker_startup(
        &self,
        run_id: &str,
        mut child: Child,
        workspace: &Path,
        worker_log: &Path,
        mut worker_log_reader: File,
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        let child_pid = child.id();
        let mut claimed = false;
        loop {
            #[cfg(test)]
            worker_observer_read_counter::record(self, run_id);
            let run = self
                .get_job_run_backend(run_id)?
                .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
            if run.pid == Some(child_pid) && !claimed {
                let _ = self.record_pipeline_audit(
                    "pipeline.worker.claimed",
                    Some(run_id),
                    actor,
                    AuditEventStatus::Success,
                    json!({
                        "run_id": run_id,
                        "worker_pid": child_pid,
                        "owner_pid": child_pid,
                        "workspace": workspace,
                        "worker_log": worker_log,
                        "state": run.state.to_string(),
                    }),
                    None,
                );
                claimed = true;
            }

            // A persisted owner or non-pending state settles the only startup
            // question this observer owns. Waiting for the child avoids a
            // full run/step SQLite read every 25ms throughout normal work.
            let status = if run.pid.is_some() || run.state != JobRunState::Pending {
                Some(child.wait().map_err(|error| {
                    OrbitError::Execution(format!(
                        "wait for pipeline worker process for run '{run_id}': {error}"
                    ))
                })?)
            } else {
                child.try_wait().map_err(|error| {
                    OrbitError::Execution(format!(
                        "observe pipeline worker process for run '{run_id}': {error}"
                    ))
                })?
            };

            if let Some(status) = status {
                // The worker may have changed the run after the last startup
                // observation. Exit handling must use fresh state so duplicate
                // ownership, cancellation, and terminal outcomes stay
                // authoritative.
                #[cfg(test)]
                worker_observer_read_counter::record(self, run_id);
                let run = self.get_job_run_backend(run_id)?.ok_or_else(|| {
                    OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string())
                })?;
                let output = read_pipeline_worker_log_tail(&mut worker_log_reader);
                let output_detail = output
                    .as_deref()
                    .filter(|value| !value.is_empty())
                    .map(|value| format!("; worker output:\n{value}"))
                    .unwrap_or_default();
                // [ORB-11116] A second worker can lose the atomic Start race
                // and exit successfully while the incumbent's PID remains on
                // the run. Only this observer's exact child PID establishes
                // ownership; another non-null PID is a benign duplicate
                // delivery, not evidence that this child abandoned the run.
                if let Some(owner_pid) = run.pid.filter(|owner_pid| *owner_pid != child_pid) {
                    tracing::info!(
                        target: "orbit.core.job_run",
                        run_id,
                        worker_pid = child_pid,
                        owner_pid,
                        exit_status = %status,
                        "duplicate pipeline worker exited without owning the persisted run",
                    );
                    let _ = self.record_pipeline_audit(
                        "pipeline.worker.duplicate",
                        Some(run_id),
                        actor,
                        AuditEventStatus::Success,
                        json!({
                            "run_id": run_id,
                            "worker_pid": child_pid,
                            "owner_pid": owner_pid,
                            "workspace": workspace,
                            "worker_log": worker_log,
                            "state": run.state.to_string(),
                            "exit_status": status.to_string(),
                        }),
                        None,
                    );
                    return Ok(());
                }
                #[cfg(unix)]
                if let Some(signal) = status
                    .signal()
                    .filter(|signal| matches!(*signal, libc::SIGTERM | libc::SIGKILL))
                    && self.record_pipeline_worker_cancellation_exit(
                        &run,
                        signal,
                        &status.to_string(),
                        actor,
                    )?
                {
                    // The cancelling caller owns terminalization after it has
                    // verified both the recorded leader and process group are
                    // gone. Reaping the worker proves only the leader exited;
                    // finalizing here could release reservations while a
                    // run-owned child remains alive.
                    return Ok(());
                }
                if run.state.is_terminal() {
                    return Ok(());
                }
                let ownership = if run.pid == Some(child_pid) {
                    "after claiming"
                } else {
                    "before claiming"
                };
                let message = format!(
                    "pipeline worker for run '{run_id}' exited with status {status} {ownership} \
                     the persisted run from registered workspace '{}'; worker log: \
                     '{}'{output_detail}; verify workspace registration, worker root discovery, \
                     and action availability",
                    workspace.display(),
                    worker_log.display(),
                );
                self.finalize_pipeline_worker_exit_failure(&run, &message, actor)?;
                return Ok(());
            }

            thread::sleep(Duration::from_millis(25));
        }
    }
    /// Record a TERM/KILL worker exit that belongs to an outstanding
    /// cancellation request, without terminalizing the run. The signalling
    /// caller performs the authoritative liveness verification and then
    /// finalizes `cancelled`; this observer only preserves the completion
    /// cause and suppresses the misleading generic worker-failure path.
    #[cfg(unix)]
    pub(crate) fn record_pipeline_worker_cancellation_exit(
        &self,
        run: &JobRun,
        signal: i32,
        exit_status: &str,
        actor: Option<&str>,
    ) -> Result<bool, OrbitError> {
        let Some(request_id) = self.active_job_run_cancellation_request(&run.run_id)? else {
            return Ok(false);
        };
        self.record_pipeline_audit(
            CANCELLATION_WORKER_EXIT_AUDIT,
            Some(&run.run_id),
            actor,
            AuditEventStatus::Success,
            json!({
                "request_id": request_id,
                "run_id": run.run_id,
                "owner_pid": run.pid,
                "signal": signal,
                "signal_name": worker_cancellation_signal_name(signal),
                "exit_status": exit_status,
                "observed_at": Utc::now().to_rfc3339(),
            }),
            None,
        )?;
        Ok(true)
    }
    /// Terminalize a worker process that exited while it still owned a
    /// non-terminal run. `try_wait` has already reaped the process when this is
    /// called. Pending exits are interrupted startup; a worker that reached
    /// running failed its claimed execution.
    fn finalize_pipeline_worker_exit_failure(
        &self,
        run: &JobRun,
        message: &str,
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        let current = self
            .get_job_run_backend(&run.run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run.run_id.clone()))?;
        let (state, started_at, audit_name) = match current.state {
            JobRunState::Pending => (
                JobRunState::Interrupted,
                current.scheduled_at,
                "pipeline.worker.startup",
            ),
            JobRunState::Running => (
                JobRunState::Failed,
                current.started_at.unwrap_or(current.scheduled_at),
                "pipeline.worker.exit",
            ),
            _ => return Ok(()),
        };
        let finished_at = Utc::now();
        self.record_pipeline_diagnostic_step(
            &current,
            started_at,
            finished_at,
            None,
            message,
            state,
        )?;
        let changed = self.finalize_job_run_with_reservation_cleanup(
            &current.run_id,
            state,
            finished_at,
            None,
            TaskReservationReleaseReason::RunTerminal,
        )?;
        if changed {
            self.record_event(OrbitEvent::JobRunCompleted {
                job_id: current.job_id.clone(),
                run_id: current.run_id.clone(),
                state: state.to_string(),
            })?;
        }
        let worker_log = pipeline_worker_log_path(&self.paths().logs_dir, &current.run_id)?;
        self.record_pipeline_audit(
            audit_name,
            Some(&current.run_id),
            actor,
            AuditEventStatus::Failure,
            json!({
                "run_id": current.run_id,
                "workspace": self.paths().repo_root,
                "worker_log": worker_log,
            }),
            Some(message.to_string()),
        )
    }
    pub(super) fn finalize_pipeline_worker_startup_failure(
        &self,
        run: &JobRun,
        message: &str,
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        let current = self.show_job_run(&run.run_id)?;
        if current.state != JobRunState::Pending || current.pid.is_some() {
            return Ok(());
        }

        let finished_at = Utc::now();
        // Persist the diagnostic step before terminalizing the run: an observer
        // polling for a terminal state must never be able to see one without its
        // startup diagnostic already durable.
        self.record_pipeline_diagnostic_step(
            run,
            run.scheduled_at,
            finished_at,
            None,
            message,
            JobRunState::Interrupted,
        )?;
        let changed = self.finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            JobRunState::Interrupted,
            finished_at,
            None,
            TaskReservationReleaseReason::RunTerminal,
        )?;
        if changed {
            self.record_event(OrbitEvent::JobRunCompleted {
                job_id: run.job_id.clone(),
                run_id: run.run_id.clone(),
                state: JobRunState::Interrupted.to_string(),
            })?;
        }
        let worker_log = pipeline_worker_log_path(&self.paths().logs_dir, &run.run_id)?;
        self.record_pipeline_audit(
            "pipeline.worker.startup",
            Some(&run.run_id),
            actor,
            AuditEventStatus::Failure,
            json!({
                "run_id": run.run_id,
                "workspace": self.paths().repo_root,
                "worker_log": worker_log,
            }),
            Some(message.to_string()),
        )
    }
    pub(crate) fn record_pipeline_audit(
        &self,
        tool_name: &str,
        target_id: Option<&str>,
        actor: Option<&str>,
        status: AuditEventStatus,
        arguments: Value,
        error_message: Option<String>,
    ) -> Result<(), OrbitError> {
        let arguments_json = serde_json::to_string(&arguments).map_err(|error| {
            OrbitError::Store(format!("serialize pipeline audit args: {error}"))
        })?;
        let execution_id = audit_execution_id("exec");
        self.record_audit_event(&AuditEventInsertParams {
            execution_id,
            command: "tool".to_string(),
            subcommand: Some("run".to_string()),
            tool_name: Some(tool_name.to_string()),
            target_type: Some("job_run".to_string()),
            target_id: target_id.map(ToOwned::to_owned),
            role: "admin".to_string(),
            status,
            exit_code: if status == AuditEventStatus::Success {
                0
            } else {
                1
            },
            duration_ms: 0,
            working_directory: self.paths().repo_root.display().to_string(),
            arguments_json: Some(arguments_json),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message,
            host: actor.map(ToOwned::to_owned),
            pid: std::process::id(),
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_host_id: None,
            process_machine_id: None,
            process_host_id: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: None,
            job_run_id: target_id.map(ToOwned::to_owned),
            activity_id: None,
            step_index: None,
        })
    }
}

#[cfg(unix)]
fn worker_cancellation_signal_name(signal: i32) -> &'static str {
    match signal {
        libc::SIGTERM => "SIGTERM",
        libc::SIGKILL => "SIGKILL",
        _ => "unknown",
    }
}
