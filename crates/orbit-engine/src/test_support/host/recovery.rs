//! Defaults for test fixtures implementing the Recovery capability.

#[doc(hidden)]
#[macro_export]
macro_rules! __orbit_test_host_recovery {
    ($($implementation:tt)*) => {
        $crate::__orbit_test_host_impl! {
            $($implementation)*;
            fn resolve_activity_tool_denials(
                &self,
                _run_id: Option<&str>,
                _task_ids: &[String],
                _activity: &str,
                _disallow_list: &[String],
            ) -> Result<$crate::ResolvedActivityTools, $crate::DispatchError> {
                Err(fixture::unsupported_dispatch_capability(
                    "resolve_activity_tool_denials",
                ))
            }

            fn resolve_activity_tools(
                &self,
                _task_ids: &[String],
                baseline_tools: &[String],
            ) -> Result<$crate::ResolvedActivityTools, $crate::DispatchError> {
                Ok(fixture::baseline_activity_tools(baseline_tools))
            }

            fn checkpoint_step(
                &self,
                _run_id: &str,
                _step_index: u32,
                _step_id: &str,
                _output: &fixture::Value,
                _compound_outputs: &std::collections::BTreeMap<String, fixture::Value>,
                _script_outputs: &std::collections::BTreeMap<
                    String,
                    fixture::orbit_types::workflow::activity_job::ScriptOutputSet,
                >,
            ) -> Result<(), $crate::DispatchError> {
                Ok(())
            }

            /// A host without a skip marker checkpoints the skipped step like any
            /// other with a null output.
            fn checkpoint_skipped_step(
                &self,
                run_id: &str,
                step_index: u32,
                step_id: &str,
            ) -> Result<(), $crate::DispatchError> {
                $crate::Recovery::checkpoint_step(
                    self,
                    run_id,
                    step_index,
                    step_id,
                    &fixture::Value::Null,
                    &std::collections::BTreeMap::new(),
                    &std::collections::BTreeMap::new(),
                )
            }

            fn observe_auto_task_step(
                &self,
                _run_id: &str,
                _request: &$crate::AutoTaskStepRequest,
            ) -> Result<$crate::AutoTaskObservation, $crate::DispatchError> {
                Err($crate::DispatchError::HostRequired("auto-task step"))
            }

            fn checkpoint_auto_task_step(
                &self,
                _run_id: &str,
                _step_id: &str,
                _record: &fixture::orbit_types::workflow::AutoTaskStepRecord,
            ) -> Result<(), $crate::DispatchError> {
                Err($crate::DispatchError::HostRequired("auto-task step"))
            }

            fn admit_auto_task_step(
                &self,
                _run_id: &str,
                _step_id: &str,
            ) -> Result<$crate::AutoTaskAdmission, $crate::DispatchError> {
                Err($crate::DispatchError::HostRequired("auto-task step"))
            }

            fn checkpoint_parallel_branch(
                &self,
                _run_id: &str,
                _step_id: &str,
                _branch_id: &str,
                _output: &fixture::Value,
            ) -> Result<(), $crate::DispatchError> {
                Err($crate::DispatchError::JobExecution(
                    "host does not persist parallel branch checkpoints".to_string(),
                ))
            }

            fn park_job_run(
                &self,
                _run_id: &str,
                _park: &fixture::orbit_types::workflow::ParkedWait,
            ) -> Result<(), $crate::DispatchError> {
                Err($crate::DispatchError::JobExecution(
                    "host cannot park a job run".to_string(),
                ))
            }

            fn checkpoint_step_diagnostic(
                &self,
                _run_id: &str,
                _step_id: &str,
                _output: &fixture::Value,
            ) -> Result<(), $crate::DispatchError> {
                Ok(())
            }

            fn checkpoint_failure_activity(
                &self,
                _run_id: &str,
                _activity_name: &str,
                _failed_step_id: &str,
                _output: &fixture::Value,
            ) -> Result<(), $crate::DispatchError> {
                Ok(())
            }

            fn final_recovery_log_tail(
                &self,
                _run_id: &str,
            ) -> Result<Option<String>, fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability(
                    "final_recovery_log_tail",
                ))
            }

            fn admit_final_recovery(
                &self,
                _run_id: &str,
                _request: &$crate::FinalRecoveryAdmissionRequest,
            ) -> Result<$crate::FinalRecoveryAdmission, fixture::OrbitError>
            {
                Ok($crate::FinalRecoveryAdmission::Skipped {
                    reason: "this runtime host does not support final recovery".to_string(),
                })
            }

            fn completion_deadline(
                &self,
                _run_id: &str,
                _pull_request: &str,
                proposed: u64,
            ) -> Result<u64, fixture::OrbitError> {
                Ok(proposed)
            }

            fn completion_policy_digest(
                &self,
                _run_id: &str,
                _task_ids: &[String],
            ) -> Result<Option<String>, fixture::OrbitError> {
                Ok(None)
            }

            fn completion_observation_admission(
                &self,
                _run_id: &str,
                _task_id: &str,
                _step_id: &str,
            ) -> Result<
                $crate::CompletionObservationAdmission,
                fixture::OrbitError,
            > {
                Ok($crate::CompletionObservationAdmission::Unavailable)
            }

            fn authenticate_completion_tail(
                &self,
                _run_id: &str,
                _task_id: &str,
                _step_id: &str,
            ) -> Result<bool, fixture::OrbitError> {
                Ok(false)
            }

            fn apply_final_recovery(
                &self,
                _run_id: &str,
                _application: &$crate::FinalRecoveryApplication,
            ) -> Result<$crate::FinalRecoveryApplied, fixture::OrbitError>
            {
                Err(fixture::unsupported_runtime_capability(
                    "apply_final_recovery",
                ))
            }

            fn persist_final_recovery_evidence(
                &self,
                _run_id: &str,
                _task_id: &str,
                _workspace: &std::path::Path,
                _evidence: &fixture::Value,
            ) -> Result<(), fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability(
                    "persist_final_recovery_evidence",
                ))
            }

            fn begin_rebase_recovery_attempt(
                &self,
                _run_id: &str,
                _step_id: &str,
                _scope: &$crate::RebaseRecoveryAttemptScope,
            ) -> Result<u64, $crate::DispatchError> {
                Err($crate::DispatchError::JobExecution(
                    "host does not support durable rebase recovery checkpoints".to_string(),
                ))
            }

            fn checkpoint_rebase_recovery(
                &self,
                _run_id: &str,
                _step_id: &str,
                _output: &fixture::Value,
            ) -> Result<(), $crate::DispatchError> {
                Err($crate::DispatchError::JobExecution(
                    "host does not support durable rebase recovery checkpoints".to_string(),
                ))
            }

            fn verify_rebase_recovery(
                &self,
                _run_id: &str,
                _step_id: &str,
                _checkpoint: &fixture::Value,
            ) -> Result<bool, fixture::OrbitError> {
                Ok(false)
            }

            fn rebase_recovery_attempts(
                &self,
                _run_id: &str,
                _step_id: &str,
            ) -> Result<
                Vec<$crate::RebaseRecoveryAttemptScope>,
                fixture::OrbitError,
            > {
                Err(fixture::unsupported_runtime_capability(
                    "rebase_recovery_attempts",
                ))
            }

            fn tool_context_for_activity(
                &self,
                _run_id: Option<&str>,
                _fs_profile: Option<&str>,
                _fs_audit: Option<std::sync::Arc<dyn fixture::orbit_tools::FsAuditLogger>>,
                _proc_allowed_programs: Option<&[String]>,
            ) -> fixture::orbit_tools::ToolContext {
                fixture::orbit_tools::ToolContext::default()
            }

            fn begin_agent_execution(
                &self,
                _run_id: &str,
                _activity_id: &str,
                _provider: &str,
                _model: Option<&str>,
                _identity: &fixture::orbit_types::telemetry::AgentExecutionIdentity,
            ) -> Result<Option<i64>, fixture::OrbitError> {
                Ok(None)
            }

            fn persist_invocation_trace(
                &self,
                _job_run_id: &str,
                _activity_id: &str,
                _input: &fixture::Value,
                _invocation: &$crate::DispatchInvocationTrace,
            ) -> Result<(), $crate::DispatchError> {
                Ok(())
            }

            fn agent_crew_config_for_input(
                &self,
                input: &fixture::Value,
            ) -> Result<Option<$crate::CrewConfig>, $crate::DispatchError> {
                fixture::validate_input_crew(input)
            }
        }
    };
}
