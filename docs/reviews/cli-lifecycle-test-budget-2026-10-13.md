# CLI process and workspace test budget candidate

This candidate retires standalone entry points into contract tables. Every retained row keeps its original assertions and platform guards. Mutable runtime bodies run in an exact isolated child once per table. This reduces runner entries, rather than claiming that every row or CLI subprocess was removed.

One duplicated replay-policy matrix retires against core integration coverage. The hung candidate-probe case moves to the public updater and uses the existing supervision-clock seam after the real candidate signals readiness, preserving timeout, untouched executable/generation, staging/backup cleanup and free admission during and after the probe.

The original macOS-only 60-second wall wait is eliminated without shortening a startup deadline. Existing binary update argument, refusal and output cases remain. The Linux-only probe module is guarded at its declaration, removing an unused-import warning on macOS.

## Measurements

The count limits pass; the summed-time limits fail. This is an incomplete candidate, not a review-ready delivery. Repository gates were not run because runtime acceptance remains unmet.

| Binary | Historical Linux baseline | Native macOS baseline | Candidate |
| --- | --- | --- | --- |
| process | 164 tests / 330 s summed | 157 tests / 529.497 s summed | 74 tests / 488.475 s summed |
| workspace | 143 tests / 236 s summed | 136 tests / 190.213 s summed | 64 tests / 190.688 s summed |

Baseline: `env -u ORBIT_WORKER_CONTEXT_REQUIRED ./scripts/build-budget.py -- cargo nextest run -p orbit-cli --test process --test workspace --no-fail-fast --success-output immediate --tool-config-file test-budget:<baseline-config>` at `872c7d2b58b009e1502b358df5a6c795f0958084`. The two JUnit suites supply independent counts and summed testcase durations. Candidate commands use `--test process` and `--test workspace` separately on this same macOS host, with the same build-budget admission and repository profile. Build time is excluded from JUnit testcase totals.

The native process baseline reports Seatbelt and ps-denied notices. Restricted sandbox coverage is not a completed pass; these notices must remain visible in any final affected-test gate. Native platform guards account for the difference from the historical Linux counts. Existing ignored helper entry points remain ignored; no test was newly ignored.

Process test wall time is 124.636 s; workspace test wall time is 67.526 s. These are not summed testcase times. The historical limits are 165 s and 118 s; half of this host's measured baseline would be 264.749 s and 95.107 s. The candidate exceeds both comparisons. Native benchmarks used this checkout's ordinary nextest profile and build-budget admission. The discarded four-thread and single-thread scheduling experiments did not meet the goals; the profile is unchanged.

Final commands: `cargo nextest run -p orbit-cli --test process --no-fail-fast --success-output immediate --tool-config-file test-budget:<process-config>` and the same command with `--test workspace` / `<workspace-config>`, each under `env -u ORBIT_WORKER_CONTEXT_REQUIRED ./scripts/build-budget.py --`. Logs and JUnit reports are under `.orbit/tmp/test-budget/`; `measurements.json` records the current host, head, counts, summed times and sandbox notices.

Validation: both full binary measurements exited 0; `cargo fmt --all -- --check` and `git diff --check` passed. `make ci-fast`, `make ci-test-affected`, `make ci-lint`, `make goldens` and `make check-ci-macos` were not run. Native CI filters still name the retained `generation_root::` and `run_observation::` modules, but their selection has not been validated by the gate. Local CodeQL does not apply to this test-only change.

## Retirement map for the PR body

Each line maps one old standalone identity to its surviving table and same-named row, or gives the redundancy reason. Both the function and the row retain their original platform guards.

### process

| Previous entry point | Surviving coverage / reason |
| --- | --- |
| `run_observation::successful_security_sweep_shows_filing_floor_source_and_excluded_alerts` | `run_observation::run_observation_contracts_0` / `successful_security_sweep_shows_filing_floor_source_and_excluded_alerts` |
| `run_observation::shipped_security_job_uses_config_unless_run_input_overrides_it` | `run_observation::run_observation_contracts_0` / `shipped_security_job_uses_config_unless_run_input_overrides_it` |
| `run_observation::readiness_bootstrap_preserves_stale_runs_and_held_reservations` | `run_observation::run_observation_contracts_0` / `readiness_bootstrap_preserves_stale_runs_and_held_reservations` |
| `run_observation::readiness_reports_total_and_notifies_only_when_backlog_is_truncated` | `run_observation::run_observation_contracts_0` / `readiness_reports_total_and_notifies_only_when_backlog_is_truncated` |
| `run_observation::job_replay_cli_reexecutes_deterministic_fixture_with_persisted_lineage` | `run_observation::run_observation_contracts_1` / `job_replay_cli_reexecutes_deterministic_fixture_with_persisted_lineage` |
| `run_observation::no_reconcile_run_reads_leave_stale_runs_and_reservations_unchanged` | `run_observation::run_observation_contracts_1` / `no_reconcile_run_reads_leave_stale_runs_and_reservations_unchanged` |
| `run_observation::default_run_reads_still_reconcile_stale_runs` | `run_observation::run_observation_contracts_1` / `default_run_reads_still_reconcile_stale_runs` |
| `run_observation::explicit_run_lookup_preserves_store_errors` | `run_observation::run_observation_contracts_1` / `explicit_run_lookup_preserves_store_errors` |
| `run_observation::job_run_alias_produces_a_completed_trace_and_terminal_cancel_is_stable` | `run_observation::run_observation_contracts_2` / `job_run_alias_produces_a_completed_trace_and_terminal_cancel_is_stable` |
| `run_observation::cancel_task_leaf_requeues_by_default_and_block_is_explicit` | `run_observation::run_observation_contracts_2` / `cancel_task_leaf_requeues_by_default_and_block_is_explicit` |
| `run_observation::cancelling_a_live_run_keeps_the_signalled_worker_exit_as_its_outcome` | `run_observation::run_observation_contracts_2` / `cancelling_a_live_run_keeps_the_signalled_worker_exit_as_its_outcome` |
| `run_observation::replacement_drain_counts_and_reports_inherited_workers_until_they_finish` | `run_observation::run_observation_contracts_2` / `replacement_drain_counts_and_reports_inherited_workers_until_they_finish` |
| `run_observation::run_concurrency_updates_persisted_revision_and_refuses_stale_writes` | `run_observation::run_observation_contracts_3` / `run_concurrency_updates_persisted_revision_and_refuses_stale_writes` |
| `run_observation::run_extend_lengthens_a_live_drain_and_a_repeat_start_still_queues` | `run_observation::run_observation_contracts_3` / `run_extend_lengthens_a_live_drain_and_a_repeat_start_still_queues` |
| `run_observation::a_drain_at_concurrency_twenty_runs_twenty_delivery_wrappers_at_once` | `run_observation::run_observation_contracts_3` / `a_drain_at_concurrency_twenty_runs_twenty_delivery_wrappers_at_once` |
| `run_observation::run_concurrency_retunes_a_pull_drain_past_the_former_leaf_ceiling` | `run_observation::run_observation_contracts_3` / `run_concurrency_retunes_a_pull_drain_past_the_former_leaf_ceiling` |
| `run_observation::a_live_drains_recorded_throttle_reaches_readiness_run_show_and_ship` | `run_observation::run_observation_contracts_4` / `a_live_drains_recorded_throttle_reaches_readiness_run_show_and_ship` |
| `run_observation::run_show_counts_local_deferred_tasks_once` | `run_observation::run_observation_contracts_4` / `run_show_counts_local_deferred_tasks_once` |
| `run_observation::run_show_names_the_tasks_a_pull_drains_owner_kept_off_this_host` | `run_observation::run_observation_contracts_4` / `run_show_names_the_tasks_a_pull_drains_owner_kept_off_this_host` |
| `run_observation::run_show_reports_deferred_tasks_beyond_the_listed_ones` | `run_observation::run_observation_contracts_4` / `run_show_reports_deferred_tasks_beyond_the_listed_ones` |
| `run_observation::run_show_names_the_pull_breaker` | `run_observation::run_observation_contracts_5` / `run_show_names_the_pull_breaker` |
| `run_observation::run_show_exposes_degraded_pull_pass_health` | `run_observation::run_observation_contracts_5` / `run_show_exposes_degraded_pull_pass_health` |
| `run_observation::doctor_warns_when_the_replicas_gates_differ_from_the_owners` | `run_observation::run_observation_contracts_5` / `doctor_warns_when_the_replicas_gates_differ_from_the_owners` |
| `run_observation::force_cancel_reports_unstopped_local_children_and_exits_one` | `run_observation::run_observation_contracts_5` / `force_cancel_reports_unstopped_local_children_and_exits_one` |
| `run_observation::a_surface_reservation_reaches_readiness_and_drain_run_show` | `run_observation::run_observation_contracts_6` / `a_surface_reservation_reaches_readiness_and_drain_run_show` |
| `run_observation::run_show_marks_an_agent_blocked_on_a_stopped_descendant` | `run_observation::run_observation_contracts_6` / `run_show_marks_an_agent_blocked_on_a_stopped_descendant` |
| `run_observation::build_budget::readiness_and_doctor_warn_for_local_and_pull_drain_capacity` | `run_observation::build_budget::build_capacity_contracts` / `readiness_and_doctor_warn_for_local_and_pull_drain_capacity` |
| `run_observation::build_budget::readiness_and_doctor_stay_silent_for_a_checkout_outside_the_admission_wrapper` | `run_observation::build_budget::build_capacity_contracts` / `readiness_and_doctor_stay_silent_for_a_checkout_outside_the_admission_wrapper` |
| `run_observation::build_budget::readiness_stays_silent_when_the_wrapper_link_resolves_outside_the_checkout` | `run_observation::build_budget::build_capacity_contracts` / `readiness_stays_silent_when_the_wrapper_link_resolves_outside_the_checkout` |
| `run_observation::build_budget::invalid_build_slots_setting_surfaces_as_advisory_in_readiness_and_doctor` | `run_observation::build_budget::build_capacity_contracts` / `invalid_build_slots_setting_surfaces_as_advisory_in_readiness_and_doctor` |
| `run_observation::build_budget::leaf_show_retains_failed_attempt_and_live_wait_statistics` | `run_observation::build_budget::build_capacity_contracts` / `leaf_show_retains_failed_attempt_and_live_wait_statistics` |
| `run_observation::run_show_display::run_show_prints_human_durations_and_one_based_steps_in_every_human_mode` | `run_observation::run_show_display::run_show_contracts` / `run_show_prints_human_durations_and_one_based_steps_in_every_human_mode` |
| `run_observation::run_show_display::run_show_hides_shipped_catalog_layers_unless_verbose` | `run_observation::run_show_display::run_show_contracts` / `run_show_hides_shipped_catalog_layers_unless_verbose` |
| `run_observation::run_show_display::run_show_lists_audit_steps_for_an_interrupted_run_with_only_a_run_level_record` | `run_observation::run_show_display::run_show_contracts` / `run_show_lists_audit_steps_for_an_interrupted_run_with_only_a_run_level_record` |
| `run_observation::run_show_display::every_run_view_reads_the_step_an_interrupted_run_died_in_as_interrupted` | `run_observation::run_show_display::run_show_contracts` / `every_run_view_reads_the_step_an_interrupted_run_died_in_as_interrupted` |
| `run_observation::run_show_display::run_show_lists_a_final_recovery_resume_with_its_rationale` | `run_observation::run_show_display::run_show_contracts` / `run_show_lists_a_final_recovery_resume_with_its_rationale` |
| `run_observation::agent_invoke::agent_wait_redacts_sensitive_warning_content_in_progress_and_output` | `run_observation::agent_invoke::agent_invocation_contracts_0` / `agent_wait_redacts_sensitive_warning_content_in_progress_and_output` |
| `run_observation::agent_invoke::agent_wait_prints_the_answer_and_exits_nonzero_for_failed_invocations` | `run_observation::agent_invoke::agent_invocation_contracts_0` / `agent_wait_prints_the_answer_and_exits_nonzero_for_failed_invocations` |
| `run_observation::agent_invoke::agent_timeout_admission_and_engine_use_the_same_ceiling` | `run_observation::agent_invoke::agent_invocation_contracts_0` / `agent_timeout_admission_and_engine_use_the_same_ceiling` |
| `run_observation::agent_invoke::agent_submissions_report_saturation_and_their_queue_position` | `run_observation::agent_invoke::agent_invocation_contracts_1` / `agent_submissions_report_saturation_and_their_queue_position` |
| `run_observation::agent_invoke::run_logs_follow_emits_live_output_once_and_stops_at_terminal` | `run_observation::agent_invoke::agent_invocation_contracts_1` / `run_logs_follow_emits_live_output_once_and_stops_at_terminal` |
| `run_observation::agent_invoke::run_logs_follow_exits_cleanly_when_merged_output_reader_closes_early` | `run_observation::agent_invoke::agent_invocation_contracts_1` / `run_logs_follow_exits_cleanly_when_merged_output_reader_closes_early` |
| `run_observation::job_exit_status::failing_replay_exits_nonzero_and_preserves_its_result` | `run_observation::job_exit_status::job_exit_contracts` / `failing_replay_exits_nonzero_and_preserves_its_result` |
| `run_observation::job_exit_status::held_run_resume_and_replay_exit_nonzero` | `run_observation::job_exit_status::job_exit_contracts` / `held_run_resume_and_replay_exit_nonzero` |
| `run_observation::wait::wait_expiry_reports_active_state_without_timing_out_the_run` | `run_observation::wait::wait_contracts` / `wait_expiry_reports_active_state_without_timing_out_the_run` |
| `run_observation::wait::invalid_wait_deadline_does_not_submit_work` | `run_observation::wait::wait_contracts` / `invalid_wait_deadline_does_not_submit_work` |
| `job_resume_detached::unix::ship_and_auto_cli_commands_launch_workers` | `job_resume_detached::unix::worker_command_contracts_0` / `ship_and_auto_cli_commands_launch_workers` |
| `job_resume_detached::unix::auto_refuses_approve_proposed_with_pull` | `job_resume_detached::unix::worker_command_contracts_0` / `auto_refuses_approve_proposed_with_pull` |
| `job_resume_detached::unix::auto_pull_validates_its_crew_restriction_before_submitting` | `job_resume_detached::unix::worker_command_contracts_0` / `auto_pull_validates_its_crew_restriction_before_submitting` |
| `job_resume_detached::unix::auto_warns_when_required_validation_cannot_use_the_login_shell` | `job_resume_detached::unix::worker_command_contracts_1` / `auto_warns_when_required_validation_cannot_use_the_login_shell` |
| `job_resume_detached::unix::gate_releases_reservation_after_child_failure_or_success` | `job_resume_detached::unix::worker_command_contracts_1` / `gate_releases_reservation_after_child_failure_or_success` |
| `job_resume_detached::unix::run_show_step_output_survives_an_earlier_when_skipped_step` | `job_resume_detached::unix::worker_command_contracts_1` / `run_show_step_output_survives_an_earlier_when_skipped_step` |
| `job_resume_detached::unix::resume_returns_while_detached_worker_continues_after_cli_exit` | `job_resume_detached::unix::worker_command_contracts_2` / `resume_returns_while_detached_worker_continues_after_cli_exit` |
| `job_resume_detached::unix::resume_wait_joins_detached_run_and_reports_success` | `job_resume_detached::unix::worker_command_contracts_2` / `resume_wait_joins_detached_run_and_reports_success` |
| `job_resume_detached::unix::resume_wait_exits_nonzero_for_a_failed_detached_run` | `job_resume_detached::unix::worker_command_contracts_2` / `resume_wait_exits_nonzero_for_a_failed_detached_run` |
| `job_resume_detached::lifecycle::completion_resume_restores_review_and_completes_merged_pr` | `job_resume_detached::lifecycle::resume_lifecycle_contracts_0` / `completion_resume_restores_review_and_completes_merged_pr` |
| `job_resume_detached::lifecycle::resumed_terminal_runs_block_readmitted_tasks` | `job_resume_detached::lifecycle::resume_lifecycle_contracts_0` / `resumed_terminal_runs_block_readmitted_tasks` |
| `job_resume_detached::lifecycle::completion_resume_cannot_invent_review_or_reverse_withdrawal` | `job_resume_detached::lifecycle::resume_lifecycle_contracts_0` / `completion_resume_cannot_invent_review_or_reverse_withdrawal` |
| `job_resume_detached::lifecycle::resume_preserves_promotion_across_non_completion_tails` | `job_resume_detached::lifecycle::resume_lifecycle_contracts_1` / `resume_preserves_promotion_across_non_completion_tails` |
| `job_resume_detached::lifecycle::resume_readmits_lineage_failure_handoff_blocks` | `job_resume_detached::lifecycle::resume_lifecycle_contracts_1` / `resume_readmits_lineage_failure_handoff_blocks` |
| `update::update_preserves_explicit_and_environment_roots_from_another_checkout` | `update::update_command_contracts_0` / `update_preserves_explicit_and_environment_roots_from_another_checkout` |
| `update::update_without_a_root_override_retains_default_workspace_routing` | `update::update_command_contracts_0` / `update_without_a_root_override_retains_default_workspace_routing` |
| `update::update_and_preflight_admit_against_the_same_overridden_root` | `update::update_command_contracts_0` / `update_and_preflight_admit_against_the_same_overridden_root` |
| `update::a_live_host_global_pin_refuses_an_overridden_root_update_in_either_spelling` | `update::update_command_contracts_0` / `a_live_host_global_pin_refuses_an_overridden_root_update_in_either_spelling` |
| `update::help_documents_both_latest_and_explicit_version_selection` | `update::update_command_contracts_1` / `help_documents_both_latest_and_explicit_version_selection` |
| `update::check_reports_the_available_release_and_exits_three` | `update::update_command_contracts_1` / `check_reports_the_available_release_and_exits_three` |
| `update::check_renders_a_prefix_free_remediation_sentence` | `update::update_command_contracts_1` / `check_renders_a_prefix_free_remediation_sentence` |
| `update::check_is_quiet_and_succeeds_when_the_installed_release_is_current` | `update::update_command_contracts_1` / `check_is_quiet_and_succeeds_when_the_installed_release_is_current` |
| `update::check_succeeds_without_recommending_an_update_when_the_published_release_is_older` | `update::update_command_contracts_2` / `check_succeeds_without_recommending_an_update_when_the_published_release_is_older` |
| `update::applying_an_update_to_a_checkout_build_is_refused_with_the_command_that_works` | `update::update_command_contracts_2` / `applying_an_update_to_a_checkout_build_is_refused_with_the_command_that_works` |
| `update::installed_and_unknown_location_binaries_are_not_checkout_builds` | `update::update_command_contracts_2` / `installed_and_unknown_location_binaries_are_not_checkout_builds` |
| `update::an_unparseable_requested_version_is_rejected_before_any_network_or_disk_work` | `update::update_command_contracts_2` / `an_unparseable_requested_version_is_rejected_before_any_network_or_disk_work` |
| `update::integrity::a_checksum_mismatch_never_reaches_the_install_path_and_staging_is_cleaned` | `update::integrity::release_integrity_contracts_0` / `a_checksum_mismatch_never_reaches_the_install_path_and_staging_is_cleaned` |
| `update::integrity::a_manifest_signed_over_different_bytes_is_rejected_before_download` | `update::integrity::release_integrity_contracts_0` / `a_manifest_signed_over_different_bytes_is_rejected_before_download` |
| `update::integrity::a_revoked_key_fails_closed_before_expiry` | `update::integrity::release_integrity_contracts_0` / `a_revoked_key_fails_closed_before_expiry` |
| `update::integrity::an_archive_with_extra_members_including_traversal_is_rejected` | `update::integrity::release_integrity_contracts_1` / `an_archive_with_extra_members_including_traversal_is_rejected` |
| `update::integrity::a_mislabeled_release_is_rejected_without_attempting_the_swap` | `update::integrity::release_integrity_contracts_1` / `a_mislabeled_release_is_rejected_without_attempting_the_swap` |
| `update::integrity::a_downgrade_is_refused_unless_explicitly_requested` | `update::integrity::release_integrity_contracts_1` / `a_downgrade_is_refused_unless_explicitly_requested` |
| `update::integrity::a_second_concurrent_update_is_refused` | `update::integrity::release_integrity_contracts_2` / `a_second_concurrent_update_is_refused` |
| `update::integrity::a_stale_writer_and_an_older_binary_never_displace_a_newer_install` | `update::integrity::release_integrity_contracts_2` / `a_stale_writer_and_an_older_binary_never_displace_a_newer_install` |
| `update::integrity::an_update_waits_for_an_in_flight_clock_tick_then_installs` | `update::integrity::release_integrity_contracts_2` / `an_update_waits_for_an_in_flight_clock_tick_then_installs` |
| `update::integrity::an_update_refuses_a_clock_tick_that_outlasts_the_quiesce_bound` | `update::integrity::release_integrity_contracts_3` / `an_update_refuses_a_clock_tick_that_outlasts_the_quiesce_bound` |
| `update::integrity::a_release_update_admits_a_session_that_hands_over_and_pins_after_it_does` | `update::integrity::release_integrity_contracts_3` / `a_release_update_admits_a_session_that_hands_over_and_pins_after_it_does` |
| `update::integrity::a_hanging_candidate_probe_times_out_without_holding_an_authority` | `update::flow::update_flow_contracts_0` / `a_hanging_candidate_probe_times_out_without_holding_an_authority`: Moved to the public updater with readiness-triggered supervision clock; retains timeout, no replacement, staging/backup cleanup and free admission during/after the probe. |
| `update::local_candidate::an_older_installation_is_bootstrapped_to_an_equal_version_candidate_and_replays_idempotently` | `update::local_candidate::local_candidate_contracts_0` / `an_older_installation_is_bootstrapped_to_an_equal_version_candidate_and_replays_idempotently` |
| `update::local_candidate::wrong_provenance_digest_target_or_ownership_refuses_before_replacement` | `update::local_candidate::local_candidate_contracts_0` / `wrong_provenance_digest_target_or_ownership_refuses_before_replacement` |
| `update::local_candidate::a_live_discovered_workspace_client_blocks_a_local_candidate_update` | `update::local_candidate::local_candidate_contracts_0` / `a_live_discovered_workspace_client_blocks_a_local_candidate_update` |
| `update::local_candidate::a_candidate_without_the_admission_contract_is_refused_before_replacement` | `update::local_candidate::local_candidate_contracts_1` / `a_candidate_without_the_admission_contract_is_refused_before_replacement` |
| `update::local_candidate::clients_run_but_no_update_enters_mid_validation_and_the_accepted_bytes_are_installed` | `update::local_candidate::local_candidate_contracts_1` / `clients_run_but_no_update_enters_mid_validation_and_the_accepted_bytes_are_installed` |
| `update::local_candidate::a_post_swap_failure_recovers_from_a_quoted_install_dir` | `update::local_candidate::local_candidate_contracts_1` / `a_post_swap_failure_recovers_from_a_quoted_install_dir` |
| `update::flow::oversized_mirror_inputs_fail_before_executable_replacement` | `update::flow::update_flow_contracts_0` / `oversized_mirror_inputs_fail_before_executable_replacement` |
| `update::flow::successful_read_only_inspection_cannot_authorize_a_downgrade` | `update::flow::update_flow_contracts_0` / `successful_read_only_inspection_cannot_authorize_a_downgrade` |
| `update::flow::a_replacement_without_admission_protocol_is_refused_before_installation` | `update::flow::update_flow_contracts_0` / `a_replacement_without_admission_protocol_is_refused_before_installation` |
| `update::flow::an_update_refreshes_an_installed_bundled_bubblewrap_with_the_new_release` | `update::flow::update_flow_contracts_1` / `an_update_refreshes_an_installed_bundled_bubblewrap_with_the_new_release` |
| `update::flow::recovery_hint_targets_requested_version_when_newer_release_is_published` | `update::flow::update_flow_contracts_1` / `recovery_hint_targets_requested_version_when_newer_release_is_published` |
| `update::flow::recovery_hint_preserves_allow_downgrade_when_retrying_older_release` | `update::flow::update_flow_contracts_1` / `recovery_hint_preserves_allow_downgrade_when_retrying_older_release` |
| `update::bundled_bwrap::a_signed_listed_binary_is_staged_executable_with_its_digest` | `update::bundled_bwrap::bundled_bwrap_contracts` / `a_signed_listed_binary_is_staged_executable_with_its_digest` |
| `update::bundled_bwrap::unsigned_tampered_or_unlisted_binaries_are_refused` | `update::bundled_bwrap::bundled_bwrap_contracts` / `unsigned_tampered_or_unlisted_binaries_are_refused` |
| `update::local_candidate_inputs::executable_headers_map_to_release_targets_or_refuse` | `update::local_candidate_inputs::candidate_input_contracts` / `executable_headers_map_to_release_targets_or_refuse` |
| `update::local_candidate_inputs::manifests_are_v1_operator_attested_documents_only` | `update::local_candidate_inputs::candidate_input_contracts` / `manifests_are_v1_operator_attested_documents_only` |
| `worktree_resolution::tool_run_explicit_workspace_resolves_from_home_and_temp_directory` | `worktree_resolution::checkout_routing_contracts_0` / `tool_run_explicit_workspace_resolves_from_home_and_temp_directory` |
| `worktree_resolution::config_show_reports_shared_and_local_roots_for_git_worktrees_and_overrides` | `worktree_resolution::checkout_routing_contracts_0` / `config_show_reports_shared_and_local_roots_for_git_worktrees_and_overrides` |
| `worktree_resolution::submodule_bootstrap_and_runtime_keep_the_submodule_workspace` | `worktree_resolution::checkout_routing_contracts_0` / `submodule_bootstrap_and_runtime_keep_the_submodule_workspace` |
| `worktree_resolution::submodule_roots_remain_independent_when_git_is_unavailable` | `worktree_resolution::checkout_routing_contracts_0` / `submodule_roots_remain_independent_when_git_is_unavailable` |
| `worktree_resolution::submodule_runtime_respects_its_registered_data_directory` | `worktree_resolution::checkout_routing_contracts_1` / `submodule_runtime_respects_its_registered_data_directory` |
| `worktree_resolution::linked_worktree_gitfile_fallback_checks_the_common_directory` | `worktree_resolution::checkout_routing_contracts_1` / `linked_worktree_gitfile_fallback_checks_the_common_directory` |
| `worktree_resolution::workspace_list_skips_unchanged_registry_lock_and_refuses_unpersisted_validation` | `worktree_resolution::checkout_routing_contracts_1` / `workspace_list_skips_unchanged_registry_lock_and_refuses_unpersisted_validation` |
| `worktree_resolution::auto_task_show_reports_and_honors_the_selected_definition_source` | `worktree_resolution::checkout_routing_contracts_1` / `auto_task_show_reports_and_honors_the_selected_definition_source` |
| `worktree_resolution::auto_task_crud_from_linked_worktree_writes_definition_yaml_to_local_root` | `worktree_resolution::checkout_routing_contracts_2` / `auto_task_crud_from_linked_worktree_writes_definition_yaml_to_local_root` |
| `worktree_resolution::detached_worker_bootstraps_and_claims_while_registry_writer_is_held` | `worktree_resolution::checkout_routing_contracts_2` / `detached_worker_bootstraps_and_claims_while_registry_writer_is_held` |
| `worktree_resolution::doctor_graph_cleanup_uses_split_roots_and_keeps_json_stdout_clean` | `worktree_resolution::checkout_routing_contracts_2` / `doctor_graph_cleanup_uses_split_roots_and_keeps_json_stdout_clean` |
| `worktree_resolution::doctor_stale_lock_repair_keeps_locks_a_live_process_holds` | `worktree_resolution::checkout_routing_contracts_2` / `doctor_stale_lock_repair_keeps_locks_a_live_process_holds` |
| `worktree_resolution::doctor_graph_cleanup_never_deletes_through_symlinks` | `worktree_resolution::checkout_routing_contracts_3` / `doctor_graph_cleanup_never_deletes_through_symlinks` |
| `worktree_resolution::root_resolution_precedence` | `worktree_resolution::checkout_routing_contracts_3` / `root_resolution_precedence` |
| `worktree_resolution::doctor_providers_with_explicit_root_does_not_bind_parent_of_data_dir` | `worktree_resolution::checkout_routing_contracts_3` / `doctor_providers_with_explicit_root_does_not_bind_parent_of_data_dir` |
| `worktree_resolution::clean_root_workspace_init_then_auto_task_list_succeeds` | `worktree_resolution::checkout_routing_contracts_3` / `clean_root_workspace_init_then_auto_task_list_succeeds` |
| `worktree_resolution::workspace_init_force_rebinds_synthetic_data_dir_checkout` | `worktree_resolution::checkout_routing_contracts_4` / `workspace_init_force_rebinds_synthetic_data_dir_checkout` |
| `env_pass_warning::job_start_warns_once_naming_only_the_unset_variable_and_records_it` | `env_pass_warning::pass_list_warning_contracts` / `job_start_warns_once_naming_only_the_unset_variable_and_records_it` |
| `env_pass_warning::drain_and_ship_starts_report_the_unset_variable_and_record_it` | `env_pass_warning::pass_list_warning_contracts` / `drain_and_ship_starts_report_the_unset_variable_and_record_it` |
| `env_pass_warning::start_with_every_pass_listed_variable_set_is_silent_and_records_nothing` | `env_pass_warning::pass_list_warning_contracts` / `start_with_every_pass_listed_variable_set_is_silent_and_records_nothing` |
| `env_pass_warning::doctor_warns_for_an_unset_pass_listed_variable_and_is_ok_when_all_are_set` | `env_pass_warning::pass_list_warning_contracts` / `doctor_warns_for_an_unset_pass_listed_variable_and_is_ok_when_all_are_set` |
| `run_observation::replay_crew::replay_recaptures_automatic_crews_and_preserves_explicit_choices` | `orbit-core::runtime::dispatch_admission::replay_crew::detached_replay_recaptures_automatic_crews_and_preserves_explicit_choices`: Redundant five-row crew-policy recapture matrix; core asserts the same automatic/default/explicit/legacy/provider-hold choices and preserved input. CLI replay success, lineage and nonzero exits remain in run_observation_contracts_1 and job_exit_contracts. |

### workspace

| Previous entry point | Surviving coverage / reason |
| --- | --- |
| `generation_root::explicit_root_spellings_select_the_same_generation_host_sweep_and_admission_roots` | `generation_root::generation_admission_contracts_0` / `explicit_root_spellings_select_the_same_generation_host_sweep_and_admission_roots` |
| `generation_root::task_list_with_tilde_orbit_root_registers_its_participant_under_home` | `generation_root::generation_admission_contracts_0` / `task_list_with_tilde_orbit_root_registers_its_participant_under_home` |
| `generation_root::clock_ticks_during_generation_hold_emit_one_dated_summary_on_resume` | `generation_root::generation_admission_contracts_0` / `clock_ticks_during_generation_hold_emit_one_dated_summary_on_resume` |
| `generation_root::clock_ticks_refused_by_a_breaking_migration_are_reported_by_doctor` | `generation_root::generation_admission_contracts_0` / `clock_ticks_refused_by_a_breaking_migration_are_reported_by_doctor` |
| `generation_root::init_with_root_succeeds_when_home_orbit_is_readonly_and_unpinned` | `generation_root::generation_admission_contracts_1` / `init_with_root_succeeds_when_home_orbit_is_readonly_and_unpinned` |
| `generation_root::init_with_isolated_home_writes_generation_lock_under_home_orbit` | `generation_root::generation_admission_contracts_1` / `init_with_isolated_home_writes_generation_lock_under_home_orbit` |
| `generation_root::preflight_with_root_refuses_an_unwritable_host_global_authority` | `generation_root::generation_admission_contracts_1` / `preflight_with_root_refuses_an_unwritable_host_global_authority` |
| `generation_root::preflight_with_root_reports_both_authorities_when_home_orbit_is_writable` | `generation_root::generation_admission_contracts_1` / `preflight_with_root_reports_both_authorities_when_home_orbit_is_writable` |
| `generation_root::preflight_on_isolated_home_is_blocked_by_a_live_exclusive_holder` | `generation_root::generation_admission_contracts_2` / `preflight_on_isolated_home_is_blocked_by_a_live_exclusive_holder` |
| `generation_root::isolated_home_refuses_a_different_executable_digest_while_a_pin_is_held` | `generation_root::generation_admission_contracts_2` / `isolated_home_refuses_a_different_executable_digest_while_a_pin_is_held` |
| `generation_root::init_with_root_ignores_a_foreign_pin_on_home_orbit` | `generation_root::generation_admission_contracts_2` / `init_with_root_ignores_a_foreign_pin_on_home_orbit` |
| `generation_root::preflight_with_root_admits_uninitialized_override_cwd_and_host_global` | `generation_root::generation_admission_contracts_2` / `preflight_with_root_admits_uninitialized_override_cwd_and_host_global` |
| `generation_root::preflight_with_orbit_root_admits_uninitialized_override_cwd_and_host_global` | `generation_root::generation_admission_contracts_3` / `preflight_with_orbit_root_admits_uninitialized_override_cwd_and_host_global` |
| `generation_root::preflight_refuses_a_live_cwd_client_for_root_and_orbit_root` | `generation_root::generation_admission_contracts_3` / `preflight_refuses_a_live_cwd_client_for_root_and_orbit_root` |
| `generation_root::preflight_with_initialized_root_does_not_add_a_different_cwd_workspace` | `generation_root::generation_admission_contracts_3` / `preflight_with_initialized_root_does_not_add_a_different_cwd_workspace` |
| `generation_root::preflight_deduplicates_a_host_global_alias_and_keeps_the_cwd_workspace` | `generation_root::generation_admission_contracts_3` / `preflight_deduplicates_a_host_global_alias_and_keeps_the_cwd_workspace` |
| `generation_root::preflight_reports_a_broken_cwd_workspace_config` | `generation_root::generation_admission_contracts_4` / `preflight_reports_a_broken_cwd_workspace_config` |
| `generation_root::update_with_uninitialized_root_still_requires_a_workspace` | `generation_root::generation_admission_contracts_4` / `update_with_uninitialized_root_still_requires_a_workspace` |
| `host_registry::tilde_root_host_list_matches_the_absolute_root_with_a_registered_remote` | `host_registry::host_registry_contracts_0` / `tilde_root_host_list_matches_the_absolute_root_with_a_registered_remote` |
| `host_registry::host_add_reads_identity_from_the_remote_and_refusals_leave_the_file_untouched` | `host_registry::host_registry_contracts_0` / `host_add_reads_identity_from_the_remote_and_refusals_leave_the_file_untouched` |
| `host_registry::host_list_show_rename_and_remove_report_live_state_and_dependents` | `host_registry::host_registry_contracts_0` / `host_list_show_rename_and_remove_report_live_state_and_dependents` |
| `host_registry::doctor_never_reports_a_match_for_a_host_that_omits_its_version_and_protocol` | `host_registry::host_registry_contracts_0` / `doctor_never_reports_a_match_for_a_host_that_omits_its_version_and_protocol` |
| `host_registry::doctor_migration_command_adds_an_existing_legacy_host_and_clears_the_warning` | `host_registry::host_registry_contracts_1` / `doctor_migration_command_adds_an_existing_legacy_host_and_clears_the_warning` |
| `host_registry::legacy_removal_never_probes_the_removed_host_and_requires_retained_hosts` | `host_registry::host_registry_contracts_1` / `legacy_removal_never_probes_the_removed_host_and_requires_retained_hosts` |
| `host_registry::legacy_destinations_migrate_on_first_mutation_and_both_files_are_refused` | `host_registry::host_registry_contracts_1` / `legacy_destinations_migrate_on_first_mutation_and_both_files_are_refused` |
| `host_registry::legacy_rows_whose_machine_name_and_ssh_alias_collide_migrate_to_unique_names` | `host_registry::host_registry_contracts_1` / `legacy_rows_whose_machine_name_and_ssh_alias_collide_migrate_to_unique_names` |
| `host_routing::task_show_projections_match_the_owning_host` | `host_routing::host_routing_contracts_0` / `task_show_projections_match_the_owning_host` |
| `host_routing::cli_task_ids_route_to_the_host_their_prefix_names` | `host_routing::host_routing_contracts_0` / `cli_task_ids_route_to_the_host_their_prefix_names` |
| `host_routing::federated_mcp_routes_an_id_only_call_by_prefix` | `host_routing::host_routing_contracts_0` / `federated_mcp_routes_an_id_only_call_by_prefix` |
| `host_routing::a_routed_call_carries_the_callers_authority_and_no_more` | `host_routing::host_routing_contracts_0` / `a_routed_call_carries_the_callers_authority_and_no_more` |
| `host_routing::a_write_through_a_mirror_selector_is_refused_by_that_hosts_sole_writer_rule` | `host_routing::host_routing_contracts_1` / `a_write_through_a_mirror_selector_is_refused_by_that_hosts_sole_writer_rule` |
| `host_routing::local_task_commands_refuse_remote_selection_without_delivering_or_mutating` | `host_routing::host_routing_contracts_1` / `local_task_commands_refuse_remote_selection_without_delivering_or_mutating` |
| `host_routing::routed_task_update_refusals_offer_a_command_that_works` | `host_routing::host_routing_contracts_1` / `routed_task_update_refusals_offer_a_command_that_works` |
| `host_routing::host_flag_resolves_to_the_selector_that_host_lists` | `host_routing::host_routing_contracts_1` / `host_flag_resolves_to_the_selector_that_host_lists` |
| `host_routing::host_ssh_command_hint_quotes_metacharacters_and_roundtrips_posix_argv` | `host_routing::host_routing_contracts_2` / `host_ssh_command_hint_quotes_metacharacters_and_roundtrips_posix_argv` |
| `routine_root::routine_list_preserves_unchanged_registry_modification_time` | `routine_root::routine_command_contracts_0` / `routine_list_preserves_unchanged_registry_modification_time` |
| `routine_root::routine_list_prints_next_due_and_last_fire_in_one_zone` | `routine_root::routine_command_contracts_0` / `routine_list_prints_next_due_and_last_fire_in_one_zone` |
| `routine_root::routine_list_honors_explicit_root_over_uninitialized_home_and_environment` | `routine_root::routine_command_contracts_0` / `routine_list_honors_explicit_root_over_uninitialized_home_and_environment` |
| `routine_root::routine_list_workspace_selector_scopes_results_and_reports_empty_workspaces` | `routine_root::routine_command_contracts_0` / `routine_list_workspace_selector_scopes_results_and_reports_empty_workspaces` |
| `routine_root::routine_commands_honor_orbit_root_and_mutate_only_the_selected_root` | `routine_root::routine_command_contracts_1` / `routine_commands_honor_orbit_root_and_mutate_only_the_selected_root` |
| `routine_root::routine_pause_rejects_unknown_names_and_reports_json_for_known_ones` | `routine_root::routine_command_contracts_1` / `routine_pause_rejects_unknown_names_and_reports_json_for_known_ones` |
| `routine_root::plugin_seeded_definitions_whose_plugin_is_off_are_hidden_but_still_shown_by_name` | `routine_root::routine_command_contracts_1` / `plugin_seeded_definitions_whose_plugin_is_off_are_hidden_but_still_shown_by_name` |
| `init_force_identity::force_refuses_missing_or_invalid_identity_flags_before_deleting_the_root` | `init_force_identity::init_identity_contracts` / `force_refuses_missing_or_invalid_identity_flags_before_deleting_the_root` |
| `init_force_identity::force_with_valid_identity_flags_resets_the_root_and_creates_a_new_identity` | `init_force_identity::init_identity_contracts` / `force_with_valid_identity_flags_resets_the_root_and_creates_a_new_identity` |
| `init_force_identity::ordinary_reinit_still_ignores_identity_flags_and_preserves_the_identity` | `init_force_identity::init_identity_contracts` / `ordinary_reinit_still_ignores_identity_flags_and_preserves_the_identity` |
| `init_force_identity::force_reset_may_choose_a_new_prefix_even_after_the_old_one_minted_task_ids` | `init_force_identity::init_identity_contracts` / `force_reset_may_choose_a_new_prefix_even_after_the_old_one_minted_task_ids` |
| `init_interactive_stdin::silent_open_stdin_pipe_exits_instead_of_hanging` | `init_interactive_stdin::init_stdin_contracts_0` / `silent_open_stdin_pipe_exits_instead_of_hanging` |
| `init_interactive_stdin::drip_writing_without_newline_expires_one_prompt_budget` | `init_interactive_stdin::init_stdin_contracts_0` / `drip_writing_without_newline_expires_one_prompt_budget` |
| `init_interactive_stdin::invalid_piped_machine_name_is_asked_again_before_the_root_is_seeded` | `init_interactive_stdin::init_stdin_contracts_0` / `invalid_piped_machine_name_is_asked_again_before_the_root_is_seeded` |
| `init_interactive_stdin::oversized_unterminated_pipe_answer_is_rejected` | `init_interactive_stdin::init_stdin_contracts_0` / `oversized_unterminated_pipe_answer_is_rejected` |
| `init_interactive_stdin::closed_stdin_keeps_the_existing_message` | `init_interactive_stdin::init_stdin_contracts_1` / `closed_stdin_keeps_the_existing_message` |
| `init_interactive_stdin::piped_machine_name_and_task_prefix_still_complete_interactive_init` | `init_interactive_stdin::init_stdin_contracts_1` / `piped_machine_name_and_task_prefix_still_complete_interactive_init` |
| `workspace_selector::workspace_remove_deregisters_deleted_checkout_by_name_id_and_path` | `workspace_selector::workspace_selector_contracts_0` / `workspace_remove_deregisters_deleted_checkout_by_name_id_and_path` |
| `workspace_selector::global_workspace_flag_selects_by_name_and_id_from_a_foreign_checkout` | `workspace_selector::workspace_selector_contracts_0` / `global_workspace_flag_selects_by_name_and_id_from_a_foreign_checkout` |
| `workspace_selector::global_workspace_flag_fails_closed_on_unknown_selector` | `workspace_selector::workspace_selector_contracts_0` / `global_workspace_flag_fails_closed_on_unknown_selector` |
| `workspace_selector::global_workspace_flag_on_deleted_checkout_reports_inactive_status_and_recorded_path` | `workspace_selector::workspace_selector_contracts_0` / `global_workspace_flag_on_deleted_checkout_reports_inactive_status_and_recorded_path` |
| `workspace_selector::migrate_dry_run_honors_selected_checkout_and_confirm_uses_the_same_one` | `workspace_selector::workspace_selector_contracts_1` / `migrate_dry_run_honors_selected_checkout_and_confirm_uses_the_same_one` |
| `workspace_selector::tool_run_workspace_selection_uses_global_flag_and_input_precedence` | `workspace_selector::workspace_selector_contracts_1` / `tool_run_workspace_selection_uses_global_flag_and_input_precedence` |
| `workspace_selector::checkout_path_and_cwd_resolve_an_id_that_collides_with_another_workspace_name` | `workspace_selector::workspace_selector_contracts_1` / `checkout_path_and_cwd_resolve_an_id_that_collides_with_another_workspace_name` |
| `workspace_selector::publication_lifecycle_survives_id_collision_when_selected_by_checkout_or_name` | `workspace_selector::workspace_selector_contracts_1` / `publication_lifecycle_survives_id_collision_when_selected_by_checkout_or_name` |
| `workspace_selector::task_show_follows_the_global_task_id_and_explicit_workspace_stays_a_filter` | `workspace_selector::workspace_selector_contracts_2` / `task_show_follows_the_global_task_id_and_explicit_workspace_stays_a_filter` |
| `workspace_selector::tool_run_task_show_follows_the_global_task_id_and_explicit_workspace_stays_a_filter` | `workspace_selector::workspace_selector_contracts_2` / `tool_run_task_show_follows_the_global_task_id_and_explicit_workspace_stays_a_filter` |
| `workspace_selector::workspace_role_cli_reasserts_owner_and_replica_and_preserves_registry_on_refusal` | `workspace_selector::workspace_selector_contracts_2` / `workspace_role_cli_reasserts_owner_and_replica_and_preserves_registry_on_refusal` |
| `workspace_source_remote::force_init_binds_first_origin_and_preserves_task_partition_and_publication` | `workspace_source_remote::source_remote_contracts_0` / `force_init_binds_first_origin_and_preserves_task_partition_and_publication` |
| `workspace_source_remote::force_init_recovers_identity_with_an_unchanged_non_portable_origin` | `workspace_source_remote::source_remote_contracts_0` / `force_init_recovers_identity_with_an_unchanged_non_portable_origin` |
| `workspace_source_remote::force_init_rejects_invalid_origins_before_first_binding_and_on_retry` | `workspace_source_remote::source_remote_contracts_0` / `force_init_rejects_invalid_origins_before_first_binding_and_on_retry` |
| `workspace_source_remote::force_init_cannot_bind_first_origin_from_a_replica_or_foreign_owner` | `workspace_source_remote::source_remote_contracts_0` / `force_init_cannot_bind_first_origin_from_a_replica_or_foreign_owner` |
| `workspace_source_remote::owner_can_dry_run_apply_read_back_and_retry_source_remote_rebind` | `workspace_source_remote::source_remote_contracts_1` / `owner_can_dry_run_apply_read_back_and_retry_source_remote_rebind` |
| `workspace_source_remote::credential_bearing_rebind_is_redacted_and_does_not_mutate` | `workspace_source_remote::source_remote_contracts_1` / `credential_bearing_rebind_is_redacted_and_does_not_mutate` |
| `workspace_source_remote::source_remote_help_names_inspection_dry_run_and_rebind` | `workspace_source_remote::source_remote_contracts_1` / `source_remote_help_names_inspection_dry_run_and_rebind` |
| `workspace_sync::workspace_sync_upgrades_previous_release_automation_with_provenance_intact` | `workspace_sync::workspace_sync_contracts_0` / `workspace_sync_upgrades_previous_release_automation_with_provenance_intact` |
| `workspace_sync::retired_delivery_qa_is_absent_after_init_and_a_modified_copy_is_kept` | `workspace_sync::workspace_sync_contracts_0` / `retired_delivery_qa_is_absent_after_init_and_a_modified_copy_is_kept` |
| `workspace_sync::workspace_sync_creates_missing_defaults_preserves_operator_content_and_is_idempotent` | `workspace_sync::workspace_sync_contracts_0` / `workspace_sync_creates_missing_defaults_preserves_operator_content_and_is_idempotent` |
| `workspace_sync::a_predating_workspace_definition_overrides_a_new_default_across_sync_and_update` | `workspace_sync::workspace_sync_contracts_0` / `a_predating_workspace_definition_overrides_a_new_default_across_sync_and_update` |
| `workspace_sync::workspace_sync_outside_registered_workspace_fails_before_writing` | `workspace_sync::workspace_sync_contracts_1` / `workspace_sync_outside_registered_workspace_fails_before_writing` |
| `workspace_sync::workspace_sync_reports_a_denied_manifest_write_without_claiming_convergence` | `workspace_sync::workspace_sync_contracts_1` / `workspace_sync_reports_a_denied_manifest_write_without_claiming_convergence` |
| `workspace_sync::workspace_init_migrates_legacy_gitignore_and_reinit_is_byte_idempotent` | `workspace_sync::workspace_sync_contracts_1` / `workspace_init_migrates_legacy_gitignore_and_reinit_is_byte_idempotent` |
| `workspace_sync::relocated_root_checkout_ignores_its_delivery_worktrees` | `workspace_sync::workspace_sync_contracts_1` / `relocated_root_checkout_ignores_its_delivery_worktrees` |
| `workspace_sync::workspace_init_refuses_checkout_and_name_collisions_without_writing` | `workspace_sync::workspace_sync_contracts_2` / `workspace_init_refuses_checkout_and_name_collisions_without_writing` |
| `ship_sweep_root::ship_sweep_selects_explicit_environment_and_default_registries` | `ship_sweep_root::ship_sweep_contracts` / `ship_sweep_selects_explicit_environment_and_default_registries` |
| `ship_sweep_root::ship_sweep_preserves_unchanged_registry_modification_time` | `ship_sweep_root::ship_sweep_contracts` / `ship_sweep_preserves_unchanged_registry_modification_time` |
| `ship_sweep_root::ship_sweep_dry_run_does_not_persist_invalid_status_flip_for_missing_checkout` | `ship_sweep_root::ship_sweep_contracts` / `ship_sweep_dry_run_does_not_persist_invalid_status_flip_for_missing_checkout` |
| `sweep_root::sweep_honors_explicit_root_over_uninitialized_home` | `sweep_root::sweep_command_contracts_0` / `sweep_honors_explicit_root_over_uninitialized_home` |
| `sweep_root::sweep_honors_orbit_root_over_uninitialized_home` | `sweep_root::sweep_command_contracts_0` / `sweep_honors_orbit_root_over_uninitialized_home` |
| `sweep_root::sweep_alias_and_clock_tick_have_identical_json_output` | `sweep_root::sweep_command_contracts_0` / `sweep_alias_and_clock_tick_have_identical_json_output` |
| `sweep_root::routine_clock_is_not_a_compatibility_alias` | `sweep_root::sweep_command_contracts_0` / `routine_clock_is_not_a_compatibility_alias` |
| `sweep_root::doctor_flags_an_installed_clock_without_deadline_or_descendant_cleanup` | `sweep_root::sweep_command_contracts_1` / `doctor_flags_an_installed_clock_without_deadline_or_descendant_cleanup` |
| `worktree_gc_routing::reclaim_dry_run_reports_pattern_bytes_doctor_warns_and_confirm_preserves_the_checkout` | `worktree_gc_routing::worktree_gc_contracts` / `reclaim_dry_run_reports_pattern_bytes_doctor_warns_and_confirm_preserves_the_checkout` |
| `worktree_gc_routing::routine_dispatch_ignores_ambient_orbit_root_and_reaps_only_the_owning_workspaces_worktree` | `worktree_gc_routing::worktree_gc_contracts` / `routine_dispatch_ignores_ambient_orbit_root_and_reaps_only_the_owning_workspaces_worktree` |
| `worktree_gc_routing::direct_gc_cli_previews_refuses_dirty_and_reclaims_only_the_selected_worktree` | `worktree_gc_routing::worktree_gc_contracts` / `direct_gc_cli_previews_refuses_dirty_and_reclaims_only_the_selected_worktree` |
| `doctor_runtime::task_relations_reports_a_canonical_dangling_edge_the_index_lost` | `doctor_runtime::doctor_runtime_contracts` / `task_relations_reports_a_canonical_dangling_edge_the_index_lost` |
| `doctor_runtime::retired_graph_cleanup_unlinks_boundaries_without_following_them` | `doctor_runtime::doctor_runtime_contracts` / `retired_graph_cleanup_unlinks_boundaries_without_following_them` |
| `doctor_runtime::cleanup_preserves_a_lock_held_by_a_live_process` | `doctor_runtime::doctor_runtime_contracts` / `cleanup_preserves_a_lock_held_by_a_live_process` |
| `store_gc::audit_retention_plans_then_prunes_rows_and_unreferenced_blobs` | `store_gc::store_retention_contracts` / `audit_retention_plans_then_prunes_rows_and_unreferenced_blobs` |
| `store_gc::run_retention_drops_terminal_state_and_keeps_run_history` | `store_gc::store_retention_contracts` / `run_retention_drops_terminal_state_and_keeps_run_history` |
| `store_gc::run_retention_prunes_resource_minutes_past_thirty_days` | `store_gc::store_retention_contracts` / `run_retention_prunes_resource_minutes_past_thirty_days` |
| `store_gc::retention_job_runs_when_the_default_crew_is_disabled` | `store_gc::store_retention_contracts` / `retention_job_runs_when_the_default_crew_is_disabled` |
| `tmp_gc::previews_and_confirmation_report_bytes_and_preserve_the_directory` | `tmp_gc::tmp_gc_contracts` / `previews_and_confirmation_report_bytes_and_preserve_the_directory` |
| `tmp_gc::symlinks_are_unlinked_without_traversal_and_a_linked_tmp_root_refuses` | `tmp_gc::tmp_gc_contracts` / `symlinks_are_unlinked_without_traversal_and_a_linked_tmp_root_refuses` |
| `tmp_gc::symlink_gc_still_checks_symlinks_when_non_utf8_name_is_unsupported` | `tmp_gc::tmp_gc_contracts` / `symlink_gc_still_checks_symlinks_when_non_utf8_name_is_unsupported` |
| `tmp_gc::a_checkout_without_orbit_directory_has_nothing_to_reclaim` | `tmp_gc::tmp_gc_contracts` / `a_checkout_without_orbit_directory_has_nothing_to_reclaim` |
| `scratch_gc::prunes_entries_older_than_the_window_and_never_follows_symlinks` | `scratch_gc::scratch_gc_contracts` / `prunes_entries_older_than_the_window_and_never_follows_symlinks` |
| `scratch_gc::a_negative_window_is_refused_before_anything_is_removed` | `scratch_gc::scratch_gc_contracts` / `a_negative_window_is_refused_before_anything_is_removed` |

