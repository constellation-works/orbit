//! Defaults for test fixtures implementing the RunLifecycle capability.

#[doc(hidden)]
#[macro_export]
macro_rules! __orbit_test_host_lifecycle {
    ($($implementation:tt)*) => {
        $crate::__orbit_test_host_impl! {
            $($implementation)*;
            fn start_plugin_broker(
                &self,
                _run: &$crate::PluginBrokerRun,
            ) -> Result<
                Option<Box<dyn $crate::PluginBrokerHandle>>,
                fixture::OrbitError,
            > {
                Ok(None)
            }

            fn register_worker_pid_namespace(
                &self,
                _pid: u32,
            ) -> Result<(), fixture::OrbitError> {
                if $crate::RunLifecycle::worker_invocation(self).is_some() {
                    return Err(fixture::OrbitError::Execution(
                        "worker namespace authority unavailable".into(),
                    ));
                }
                Ok(())
            }

            fn register_worker_process(
                &self,
                _pid: u32,
            ) -> Result<(), fixture::OrbitError> {
                if $crate::RunLifecycle::worker_invocation(self).is_some() {
                    return Err(fixture::OrbitError::Execution(
                        "worker process authority unavailable".into(),
                    ));
                }
                Ok(())
            }

            fn worker_invocation(&self) -> Option<fixture::orbit_types::tool::WorkerInvocation> {
                None
            }

            fn record_direct_landing_intent(
                &self,
                _request: &fixture::orbit_types::workflow::automation::DirectLandingRequest,
            ) -> Result<(), fixture::OrbitError> {
                Ok(())
            }

            fn record_direct_landing(
                &self,
                _request: &fixture::orbit_types::workflow::automation::DirectLandingRequest,
            ) -> Result<(), fixture::OrbitError> {
                Ok(())
            }

            fn accepted_automation_coverage(
                &self,
                _task_id: &str,
            ) -> Result<
                Option<fixture::orbit_types::workflow::automation::CoverageEvidence>,
                fixture::OrbitError,
            > {
                Ok(None)
            }

            fn insert_job_run(
                &self,
                job_id: &str,
                attempt: u32,
                scheduled_at: fixture::chrono::DateTime<fixture::chrono::Utc>,
                input: Option<fixture::Value>,
                retry_source_run_id: Option<String>,
            ) -> Result<fixture::orbit_types::workflow::JobRun, fixture::OrbitError>
            {
                let _ = (job_id, attempt, scheduled_at, input, retry_source_run_id);
                Err(fixture::unsupported_runtime_capability("insert_job_run"))
            }

            fn mark_job_run_running(
                &self,
                run_id: &str,
                started_at: fixture::chrono::DateTime<fixture::chrono::Utc>,
                pid: u32,
            ) -> Result<
                fixture::orbit_types::workflow::JobRunStartOutcome,
                fixture::OrbitError,
            > {
                let _ = (run_id, started_at, pid);
                Err(fixture::unsupported_runtime_capability(
                    "mark_job_run_running",
                ))
            }

            fn complete_job_run_step(
                &self,
                run_id: &str,
                params: &fixture::orbit_store::contract::JobRunStepParams,
            ) -> Result<bool, fixture::OrbitError> {
                let _ = (run_id, params);
                Err(fixture::unsupported_runtime_capability(
                    "complete_job_run_step",
                ))
            }

            fn finalize_job_run(
                &self,
                run_id: &str,
                state: fixture::orbit_types::workflow::JobRunState,
                finished_at: fixture::chrono::DateTime<fixture::chrono::Utc>,
                duration_ms: Option<u64>,
            ) -> Result<bool, fixture::OrbitError> {
                let _ = (run_id, state, finished_at, duration_ms);
                Err(fixture::unsupported_runtime_capability("finalize_job_run"))
            }

            fn get_job_run(
                &self,
                _run_id: &str,
            ) -> Result<
                Option<fixture::orbit_types::workflow::JobRun>,
                fixture::OrbitError,
            > {
                Err(fixture::unsupported_runtime_capability("get_job_run"))
            }

            fn read_run_state(
                &self,
                _run_id: &str,
            ) -> Result<
                Option<fixture::orbit_types::workflow::PipelineState>,
                fixture::OrbitError,
            > {
                Ok(None)
            }

            fn write_run_state(
                &self,
                _run_id: &str,
                _state: &fixture::orbit_types::workflow::PipelineState,
            ) -> Result<(), fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability("write_run_state"))
            }

            fn get_task(
                &self,
                _task_id: &str,
            ) -> Result<fixture::orbit_types::task::Task, fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability("get_task"))
            }

            fn get_task_artifact(
                &self,
                _task_id: &str,
                _path: &str,
            ) -> Result<
                Option<fixture::orbit_types::task::TaskArtifact>,
                fixture::OrbitError,
            > {
                Ok(None)
            }

            /// A host without a task store has no queued admission to pass, so the
            /// observed read is its ordinary read.
            fn observe_task_artifact(
                &self,
                task_id: &str,
                path: &str,
            ) -> Result<
                Option<fixture::orbit_types::task::TaskArtifact>,
                fixture::OrbitError,
            > {
                self.get_task_artifact(task_id, path)
            }

            fn get_task_artifacts(
                &self,
                _task_id: &str,
            ) -> Result<
                Vec<fixture::orbit_types::task::TaskArtifact>,
                fixture::OrbitError,
            > {
                Ok(Vec::new())
            }

            fn get_task_comments(
                &self,
                _task_id: &str,
            ) -> Result<
                Vec<fixture::orbit_types::task::TaskComment>,
                fixture::OrbitError,
            > {
                Ok(Vec::new())
            }

            fn get_task_history(
                &self,
                _task_id: &str,
            ) -> Result<
                Vec<fixture::orbit_types::task::TaskHistoryEntry>,
                fixture::OrbitError,
            > {
                Ok(Vec::new())
            }

            fn get_dependency_task(
                &self,
                task_id: &str,
            ) -> Result<Option<fixture::orbit_types::task::Task>, fixture::OrbitError>
            {
                match $crate::RunLifecycle::get_task(self, task_id) {
                    Ok(task) => Ok(Some(task)),
                    Err(fixture::OrbitError::NotFound { .. }) => Ok(None),
                    Err(error) => Err(error),
                }
            }

            fn get_dependency_history(
                &self,
                task_id: &str,
            ) -> Result<
                Option<Vec<fixture::orbit_types::task::TaskHistoryEntry>>,
                fixture::OrbitError,
            > {
                Ok(Some($crate::RunLifecycle::get_task_history(
                    self, task_id,
                )?))
            }

            fn list_tasks_filtered(
                &self,
                status: Option<fixture::orbit_types::task::TaskStatus>,
                priority: Option<fixture::orbit_types::task::TaskPriority>,
                parent_id: Option<&str>,
                job_run_id: Option<&str>,
                external_ref: Option<&fixture::orbit_types::task::ExternalRef>,
                has_external_ref_system: Option<&str>,
            ) -> Result<Vec<fixture::orbit_types::task::Task>, fixture::OrbitError>
            {
                let _ = (
                    status,
                    priority,
                    parent_id,
                    job_run_id,
                    external_ref,
                    has_external_ref_system,
                );
                Err(fixture::unsupported_runtime_capability(
                    "list_tasks_filtered",
                ))
            }

            fn list_run_tasks(
                &self,
                run_id: &str,
            ) -> Result<Vec<fixture::orbit_types::task::Task>, fixture::OrbitError>
            {
                $crate::RunLifecycle::list_tasks_filtered(
                    self,
                    None,
                    None,
                    None,
                    Some(run_id),
                    None,
                    None,
                )
            }

            fn start_task(
                &self,
                task_id: &str,
                note: Option<String>,
                comment: Option<String>,
            ) -> Result<fixture::orbit_types::task::Task, fixture::OrbitError> {
                let _ = (task_id, note, comment);
                Err(fixture::unsupported_runtime_capability("start_task"))
            }

            fn admit_task_for_workflow(
                &self,
                _task_id: &str,
                _workflow: &str,
            ) -> Result<fixture::orbit_types::task::Task, fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability(
                    "admit_task_for_workflow",
                ))
            }

            fn update_task_from_activity(
                &self,
                task_id: &str,
                update: $crate::TaskActivityUpdate,
            ) -> Result<fixture::orbit_types::task::Task, fixture::OrbitError> {
                let _ = (task_id, update);
                Err(fixture::unsupported_runtime_capability(
                    "update_task_from_activity",
                ))
            }

            fn apply_task_automation_update(
                &self,
                task_id: &str,
                update: $crate::TaskAutomationUpdate,
            ) -> Result<(), fixture::OrbitError> {
                let _ = (task_id, update);
                Err(fixture::unsupported_runtime_capability(
                    "apply_task_automation_update",
                ))
            }

            fn reject_invalid_finding(
                &self,
                report: &fixture::orbit_types::workflow::handoff::FindingInvalidReport,
                base_branch: &str,
            ) -> Result<(), fixture::OrbitError> {
                let _ = (report, base_branch);
                Err(fixture::unsupported_runtime_capability(
                    "reject_invalid_finding",
                ))
            }

            fn widen_task_context_files(
                &self,
                task_id: &str,
                run_id: &str,
                step: fixture::orbit_types::task::ContextWideningStep,
                activity: &str,
                paths: &[String],
            ) -> Result<Vec<String>, fixture::OrbitError> {
                let _ = (task_id, run_id, step, activity, paths);
                Ok(Vec::new())
            }
        }
    };
}
