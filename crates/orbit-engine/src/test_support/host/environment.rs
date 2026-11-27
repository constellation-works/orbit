//! Defaults for test fixtures implementing the HostEnvironment capability and
//! the run settings it holds.

#[doc(hidden)]
#[macro_export]
macro_rules! __orbit_test_host_run_settings {
    ($($implementation:tt)*) => {
        $crate::__orbit_test_host_impl! {
            $($implementation)*;
            /// The default settings, with the build output bindings of this
            /// fixture's validation environment.
            fn run_settings(&self) -> fixture::Arc<$crate::RunSettings> {
                fixture::Arc::new($crate::RunSettings {
                    validation_build_output_env:
                        $crate::HostEnvironment::validation_subprocess_environment(self)
                            .build_output_env,
                    ..$crate::RunSettings::default()
                })
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __orbit_test_host_environment {
    ($($implementation:tt)*) => {
        $crate::__orbit_test_host_impl! {
            $($implementation)*;
            fn admit_owed_criteria(
                &self,
                _task_id: &str,
                _run_id: &str,
                _owed: &[fixture::orbit_types::workflow::OwedCriterion],
            ) -> Result<(), fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability(
                    "admit_owed_criteria",
                ))
            }

            fn outstanding_owed_criteria(
                &self,
                _task_id: &str,
                _run_id: Option<&str>,
            ) -> Result<Vec<usize>, fixture::OrbitError> {
                Ok(Vec::new())
            }

            fn required_validation_commands(&self) -> Vec<String> {
                Vec::new()
            }

            fn review_baseline_commands(&self) -> Vec<String> {
                Vec::new()
            }

            fn validation_gates(&self) -> fixture::orbit_types::workflow::HostValidationGates {
                fixture::orbit_types::workflow::HostValidationGates::local(
                    fixture::orbit_types::workflow::ValidationGates {
                        required_validation_commands:
                            $crate::HostEnvironment::required_validation_commands(self),
                        baseline_commands: $crate::HostEnvironment::review_baseline_commands(
                            self,
                        ),
                        light_delivery_paths: None,
                        review_skip_tags: None,
                    },
                )
            }

            fn record_validated_tree(
                &self,
                _tree: &$crate::ValidatedTree,
            ) -> Result<(), fixture::OrbitError> {
                Ok(())
            }

            fn validated_tree(
                &self,
                _run_id: &str,
            ) -> Result<Option<$crate::ValidatedTree>, fixture::OrbitError>
            {
                Ok(None)
            }

            fn agent_provider_config(&self) -> std::collections::HashMap<String, String> {
                std::collections::HashMap::new()
            }

            fn agent_subprocess_environment(
                &self,
                required_env_vars: &[&str],
            ) -> Vec<(String, String)> {
                fixture::orbit_common::security::child_env::allowlisted_child_env(
                    &fixture::orbit_common::Env::capture(),
                    &[],
                    required_env_vars,
                )
            }

            /// This host's own agent environment, overlaid with the crew's entries
            /// from the process environment.
            fn crew_agent_subprocess_environment(
                &self,
                crew_env: &[fixture::orbit_types::identity::CrewEnvVar],
                required_env_vars: &[&str],
            ) -> Vec<(String, String)> {
                let env = $crate::HostEnvironment::agent_subprocess_environment(
                    self,
                    required_env_vars,
                );
                fixture::overlay_crew_env(&fixture::orbit_common::Env::capture(), env, crew_env)
            }

            fn validation_subprocess_environment(
                &self,
            ) -> fixture::orbit_exec::ValidationEnvironment {
                fixture::orbit_exec::ValidationEnvironment::launcher(
                    $crate::HostEnvironment::agent_subprocess_environment(self, &[]),
                )
            }

            fn refresh_persistence_after_cli_provider(
                &self,
            ) -> Result<(), fixture::OrbitError> {
                Ok(())
            }

            fn record_event(
                &self,
                _event: fixture::orbit_types::record::OrbitEvent,
            ) -> Result<(), fixture::OrbitError> {
                Ok(())
            }

            fn list_job_runs_for_gc(
                &self,
            ) -> Result<
                Vec<fixture::orbit_types::workflow::JobRun>,
                fixture::OrbitError,
            > {
                Err(fixture::OrbitError::Execution(
                    "worktree GC is not implemented for this runtime host".to_string(),
                ))
            }

            fn lookup_task_for_worktree_gc(
                &self,
                _run_id: &str,
                _task_id: &str,
            ) -> $crate::WorktreeGcTaskLookup {
                $crate::WorktreeGcTaskLookup::Unresolved
            }

            fn worktree_gc_task_lookup_scope(&self, _run_id: &str) -> Option<String> {
                Some("local".to_string())
            }

            fn settled_claim_for_worktree_gc(&self, _run_id: &str) -> Option<String> {
                None
            }

            fn live_claim_for_worktree_gc(&self, _run_id: &str) -> Option<String> {
                None
            }

            fn gc_scratch(
                &self,
                _retention_hours: u64,
            ) -> Result<Option<$crate::ScratchGcReport>, fixture::OrbitError>
            {
                Ok(None)
            }

            fn cancel_job_run(
                &self,
                run_id: &str,
            ) -> Result<(), fixture::OrbitError> {
                Err(fixture::OrbitError::Execution(format!(
                    "cancel_job_run is not implemented for run '{run_id}'"
                )))
            }

            fn invocation_records(
                &self,
                _query: fixture::orbit_store::contract::InvocationQuery,
            ) -> Result<
                Vec<fixture::orbit_store::contract::InvocationRecord>,
                fixture::OrbitError,
            > {
                Ok(Vec::new())
            }

            fn record_provider_limit(
                &self,
                _observation: &fixture::orbit_types::telemetry::ProviderLimitObservation,
            ) -> Result<(), fixture::OrbitError> {
                Err(fixture::unsupported_runtime_capability(
                    "record_provider_limit",
                ))
            }

            /// A double that records limits keeps each reading; it retires nothing.
            fn record_provider_usage_reading(
                &self,
                observations: &[fixture::orbit_types::telemetry::ProviderLimitObservation],
            ) -> Result<(), fixture::OrbitError> {
                observations
                    .iter()
                    .try_for_each(|observation| self.record_provider_limit(observation))
            }

            fn activity_implementer_identity(
                &self,
                _input: &fixture::Value,
            ) -> Result<(Option<String>, Option<String>), fixture::OrbitError> {
                Ok((None, None))
            }

            fn resolved_crew_model(
                &self,
                _run_id: &str,
            ) -> Result<Option<String>, fixture::OrbitError> {
                Ok(None)
            }

            fn run_tool_with_context(
                &self,
                name: &str,
                input: fixture::Value,
                tool_context: fixture::orbit_tools::ToolContext,
            ) -> Result<fixture::Value, fixture::OrbitError> {
                let _ = (name, input, tool_context);
                Err(fixture::unsupported_runtime_capability(
                    "run_tool_with_context",
                ))
            }

            fn run_private_vcs_operation(
                &self,
                operation: &str,
                input: fixture::Value,
            ) -> Result<fixture::Value, fixture::OrbitError> {
                $crate::run_private_vcs_operation(
                    &fixture::orbit_common::Env::capture(),
                    operation,
                    &input,
                )
            }

            fn resolved_agent_model_pair(
                &self,
                agent_cli: &str,
            ) -> Option<fixture::orbit_types::identity::AgentModelPair> {
                let _ = agent_cli;
                None
            }
        }
    };
}
