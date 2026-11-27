//! Defaults for test fixtures implementing the Dispatch capability.

#[doc(hidden)]
#[macro_export]
macro_rules! __orbit_test_host_dispatch {
    ($($implementation:tt)*) => {
        $crate::__orbit_test_host_impl! {
            $($implementation)*;
            fn run_deterministic(
                &self,
                action: &str,
                config: &fixture::Value,
                input: &fixture::Value,
                tool_context: fixture::orbit_tools::ToolContext,
            ) -> Result<fixture::Value, $crate::DispatchError> {
                let _ = (config, input, tool_context);
                Err(fixture::unsupported_dispatch_capability(action))
            }

            fn has_deterministic_action(&self, _action: &str) -> bool {
                true
            }

            fn resolve_cli_executor(
                &self,
                provider: &str,
            ) -> Result<$crate::ResolvedCliExecutor, $crate::DispatchError> {
                Err(fixture::unsupported_dispatch_capability(provider))
            }

            fn run_script_step(
                &self,
                request: &$crate::ScriptStepRequest,
            ) -> $crate::ScriptStepRun {
                fixture::refuse_script_step(request)
            }

            fn verify_script_output_set(
                &self,
                _consumer_run_id: &str,
                _set: &fixture::orbit_types::workflow::activity_job::ScriptOutputSet,
            ) -> Result<(), String> {
                Err("this host keeps no script output files".to_string())
            }

            fn resolve_local_shell_executor(
                &self,
                _executor: &str,
            ) -> Result<$crate::ResolvedShellExecutor, $crate::DispatchError> {
                Ok($crate::ResolvedShellExecutor::default())
            }

            fn provider_cli_config(
                &self,
                _provider: &str,
            ) -> std::collections::HashMap<String, String> {
                std::collections::HashMap::new()
            }

            fn resolve_executor_sandbox(
                &self,
                _provider: &str,
                _fs_profile: Option<&str>,
                _subprocess_cwd: Option<&std::path::Path>,
            ) -> Result<Option<$crate::ResolvedSandbox>, $crate::DispatchError> {
                Ok(None)
            }

            fn run_deferral_probe(
                &self,
                probe: &fixture::orbit_types::workflow::DeferralProbe,
                _provider: Option<&str>,
                _fs_profile: Option<&str>,
                _cwd: &std::path::Path,
            ) -> fixture::orbit_types::workflow::NamespaceProbe {
                fixture::unavailable_deferral_probe(probe)
            }

            fn task_context_for_agent_input(
                &self,
                _run_id: &str,
                _step_id: &str,
                _input: &fixture::Value,
            ) -> Result<Option<fixture::Value>, $crate::DispatchError> {
                Ok(None)
            }

            fn acquire_deferred_footprint(
                &self,
                _run_id: &str,
                _activity_name: &str,
            ) -> Result<(), $crate::DispatchError> {
                Ok(())
            }

            fn tag_instructions_for_activity(
                &self,
                _run_id: &str,
                _activity_name: &str,
                _input: &fixture::Value,
            ) -> Result<
                Vec<fixture::orbit_types::workflow::TagInstruction>,
                $crate::DispatchError,
            > {
                Ok(Vec::new())
            }

            fn agent_corpus_for_activity(
                &self,
                _run_id: &str,
                _activity_name: &str,
            ) -> Result<
                Option<fixture::orbit_types::workflow::AgentCorpusInjection>,
                $crate::DispatchError,
            > {
                Ok(None)
            }

            fn notices_for_agent_input(
                &self,
                _run_id: &str,
                _activity_name: &str,
                _input: &fixture::Value,
            ) -> Option<fixture::orbit_types::workflow::FleetNoticeBrief> {
                None
            }

            fn experiment_overlays_for_activity(
                &self,
                _run_id: &str,
                _activity_name: &str,
                _input: &fixture::Value,
                _model: Option<&str>,
            ) -> Result<
                Vec<fixture::orbit_types::workflow::ExperimentActivityOverlay>,
                $crate::DispatchError,
            > {
                Ok(Vec::new())
            }

            fn mid_run_steering_enabled(&self) -> bool {
                false
            }

            fn record_resident_continuity_rebuilt(
                &self,
                _run_id: &str,
            ) -> Result<(), fixture::OrbitError> {
                Ok(())
            }

            fn steering_gate(
                &self,
                _run_id: &str,
                _task_id: &str,
                _candidate_source: Option<&str>,
            ) -> Result<$crate::SteeringGateReading, fixture::OrbitError> {
                Ok($crate::SteeringGateReading::Disabled)
            }

            fn admit_steering_round(
                &self,
                _run_id: &str,
                _task_id: &str,
                _outstanding: &[fixture::orbit_types::workflow::CommentLogMark],
                _candidate_source: Option<&str>,
            ) -> Result<$crate::SteeringRoundAdmission, fixture::OrbitError>
            {
                Err(fixture::unsupported_runtime_capability(
                    "admit_steering_round",
                ))
            }
        }
    };
}
