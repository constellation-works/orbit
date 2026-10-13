# Engine scenario consolidation

The retired nextest entries below remain named scenario rows. Their bodies and
assertions execute in the listed surviving family test. No behavior row was
removed as redundant. Table partitions select disjoint named rows and together
cover the original table. Each family still runs in an isolated child with a
bounded supervisor; mutable repositories, hosts and audit stores remain separate.
Pristine Git fixture templates share initialization within a family process,
then copy independent files and Git objects for each scenario.

The PR landing fixtures inject only fixed retry waits through the dev-only
`test-support` hook. Poll limits, requested delays, remote identity checks, exact
SHA pins and error assertions remain exercised. Real Git/provider subprocess
budgets and cleanup continue to use real clocks. The held-slot fixtures retain
real locks and admission heartbeats, then advance the existing supervision clock
after the wrapper signals that its queue interval earned the capped credit.

## Retired nextest entries and surviving rows

### `final_recovery::recovery_contracts_1`

- `a_resume_reruns_from_the_named_step_and_the_run_completes`
- `resume_context_is_opaque_when_the_target_inherits_job_input`
- `a_refused_deferral_reaches_the_resumed_nested_implementer_and_a_restart`
- `an_oversized_refusal_reaches_the_resumed_step_bounded_under_the_admitted_id`
- `an_oversized_refused_output_reaches_the_resumed_implementer_bounded`
- `a_settling_decision_ends_the_run_without_the_failure_handoff`

### `final_recovery::recovery_contracts_2`

- `an_escalation_still_runs_the_failure_handoff`
- `an_error_that_skips_step_recovery_still_reaches_final_recovery`
- `step_recovery_runs_first_and_final_recovery_only_once_it_is_spent`
- `final_recovery_runs_at_most_once_per_run`
- `a_run_resumed_after_a_final_recovery_resume_never_runs_it_again`
- `a_run_resumed_after_a_crash_mid_settlement_replays_the_recorded_decision`

### `final_recovery::recovery_contracts_3`

- `a_provider_authentication_failure_skips_final_recovery`
- `an_empty_crew_pool_skips_final_recovery`
- `a_resume_to_a_later_step_reruns_the_failed_contract_and_records_the_downgrade`
- `a_resume_to_an_earlier_step_keeps_the_named_target`
- `a_resume_to_an_admission_or_nonexistent_step_is_escalated`
- `a_malformed_decision_is_escalated`

### `final_recovery::recovery_contracts_4`

- `a_failure_before_the_worktree_exists_skips_final_recovery`
- `failed_recovery_dispatch_is_injected_before_final_recovery`
- `final_recovery_injects_only_its_runs_bounded_evidence_in_order`
- `final_recovery_escalates_instead_of_dropping_records_that_cannot_fit`
- `final_recovery_keeps_post_recovery_output_when_a_later_step_fails`
- `final_recovery_resume_checkpoints_the_latest_conflict_diagnostics`

### `v2_runtime::runtime_contracts_1`

- `deterministic_reference_dispatches_and_persists_audit_events`
- `a_deterministic_step_binds_its_task_and_run_from_the_dispatch_not_the_tool_args`
- `parallel_join_policy_decides_the_block_from_its_branches`
- `fan_out_collects_outputs_in_item_order_within_the_worker_cap`

### `v2_runtime::runtime_contracts_2`

- `fan_in_join_policy_decides_the_block_from_its_workers`
- `loop_iterates_until_break_failure_or_budget`
- `items_expression_rejects_non_array_json`
- `when_false_step_skips_log_below_warn_and_other_skips_keep_warn`

### `v2_runtime::runtime_contracts_3`

- `incomplete_conflict_diagnostics_survive_resumed_execution_and_success_checkpoint`
- `the_shipped_review_step_retries_then_recovers_a_failing_reviewer`
- `provider_unavailable_dispatch_errors_do_not_attempt_recovery`
- `an_implementer_blocker_ends_implement_one_without_recovery`

### `v2_runtime::runtime_contracts_4`

- `an_upgrade_refusal_blocker_ends_the_step_as_upgrade_pending`
- `an_upgrade_refusal_ends_any_step_but_a_task_blocker_only_the_implementers`
- `a_malformed_blocker_is_not_a_stop_and_the_catalog_name_is`
- `malformed_unfiled_findings_fail_implement_one_before_delivery`

### `v2_runtime::runtime_contracts_5`

- `exhausted_review_preflight_stops_the_shipped_pipeline_before_implementation`
- `fan_in_retains_position_aligned_errors_through_successful_join_and_resume`
- `a_skipped_fan_out_does_not_overwrite_a_siblings_collection_on_resume`
- `leaf_completion_records_file_and_store_waits_without_leaking_between_steps`

### `v2_worktree_lifecycle::worktree_contracts_1`

- `worktree_gc_keeps_every_protected_checkout_and_reaps_settled_ones`
- `replica_worktree_gc_reclaims_settled_claims_and_says_why_it_keeps_the_rest`
- `replica_worktree_gc_fences_unreachable_owners_by_route`
- `worktree_gc_classifies_other_worktrees_when_a_run_id_sanitizes_empty`
- `worktree_setup_keeps_a_branch_prefix_checkout_under_the_worktree_root`

### `v2_worktree_lifecycle::worktree_contracts_2`

- `worktree_setup_refuses_a_stale_checkout_and_leaves_it_untouched`
- `worktree_setup_refuses_a_checkout_with_unreadable_status_and_keeps_its_edits`
- `local_landing_precheck_uses_the_normalized_base_checkout`
- `local_landing_preserves_unrelated_untracked_files`
- `local_landing_names_only_untracked_paths_conflicting_with_the_candidate`

### `v2_worktree_lifecycle::worktree_contracts_3`

- `local_landing_still_refuses_tracked_and_unmerged_work_at_setup_and_merge`
- `a_validated_candidate_is_resumed_on_the_new_base_without_implementation`
- `a_candidate_held_for_an_upgrade_refusal_is_resumed_by_the_next_run`
- `a_candidate_held_for_a_wait_timeout_is_resumed_by_the_next_run`
- `a_conflicting_candidate_is_handed_to_the_implementer_with_its_conflict`

### `v2_worktree_lifecycle::worktree_contracts_4`

- `a_candidate_failing_validation_is_handed_to_the_implementer_with_the_output`
- `a_candidate_failing_validation_its_base_shares_is_not_handed_to_the_implementer`
- `an_implementation_failure_handoff_is_requeued_for_the_implementer`
- `resumed_known_red_candidates_must_complete_host_and_replay_coverage`
- `a_changed_spec_or_an_operator_discard_implements_fresh`

### `v2_worktree_lifecycle::worktree_contracts_5`

- `a_selector_only_edit_keeps_the_preserved_candidate`
- `an_evidence_receipt_resumes_the_held_candidate_without_the_implementer`
- `a_held_candidate_that_is_gone_or_retired_implements_fresh_with_a_typed_reason`
- `a_claimed_leaf_continues_the_candidate_its_owner_kept`
- `a_local_claim_leaf_falls_back_to_the_candidate_its_claim_kept`

### `v2_worktree_lifecycle::worktree_contracts_6`

- `a_repair_claim_resumes_its_stopped_landing_candidate_for_the_implementer`
- `git_rebase_decision_follows_the_checkout_state`
- `git_rebase_rename_delete_conflicts_remain_for_recovery`
- `conflict_recovery_leaf_completes_only_its_checkpointed_rebase`
- `a_resumed_run_recovers_the_same_step_again_as_a_new_attempt`

### `v2_worktree_lifecycle::worktree_contracts_7`

- `conflict_recovery_commits_companion_edits_with_the_resolution`
- `conflict_recovery_resolves_each_stop_of_a_multi_commit_rebase`
- `conflict_recovery_still_refuses_unresolved_conflicts_beside_companion_edits`
- `agent_changed_paths_widen_selectors_with_their_step_as_the_agent_exits`
- `provider_primary_source_edit_fails_closed_and_preserves_both_checkouts`

### `v2_worktree_lifecycle::worktree_contracts_8`

- `user_git_diff_config_preserves_dirty_fingerprints_and_recovery`
- `stationary_record_store_dirt_overlapping_the_run_fails_closed`
- `stationary_record_store_dirt_disjoint_from_the_run_stays_benign`
- `primary_dirtied_between_dispatches_of_one_run_is_not_provider_drift`
- `untracked_nested_repository_fingerprints_in_primary_and_assigned_worktree`

### `v2_worktree_lifecycle::worktree_contracts_9`

- `repeated_dirty_integrity_failures_keep_current_content_and_prune_old_attempts`

### `v2_worktree_lifecycle::absorbed_candidate::absorption_contracts_1`

- `an_identical_landed_patch_settles_before_candidate_validation`
- `a_rebase_retaining_candidate_commits_still_runs_validation`
- `an_absorbed_candidate_settles_as_verified_no_diff_without_an_agent`
- `a_discarded_candidate_reaches_the_final_recovery_agent`

### `v2_worktree_lifecycle::absorbed_candidate::absorption_contracts_2`

- `a_candidate_the_advanced_base_absorbed_is_certified_on_that_tip`
- `a_dropped_candidate_is_refused_at_the_continuation`
- `absorbed_neighbours_fail_closed_with_a_typed_reason`
- `a_quoted_absorption_marker_never_settles_a_certified_candidate`

### `v2_worktree_lifecycle::conflict_adoption::adoption_contracts_1`

- `untouched_binary_ancestor_and_custom_driver_stops_are_not_adopted`
- `earlier_repairs_of_binary_ancestor_and_custom_driver_stops_continue`
- `custom_driver_repairs_require_unchanged_stopped_output_evidence`
- `a_no_op_recovery_continues_the_rebase_from_earlier_repairs`

### `v2_worktree_lifecycle::conflict_adoption::adoption_contracts_2`

- `earlier_repairs_still_refuse_markers_whitespace_and_unrepaired_conflicts`
- `earlier_repairs_are_not_continued_after_head_branch_or_index_moved`
- `earlier_repairs_are_not_continued_for_a_mismatched_checkpoint`
- `earlier_repairs_are_not_continued_once_ownership_or_authorization_changed`

### `v2_worktree_lifecycle::dirty_rescue::rescue_contracts_1`

- `a_settled_dirty_worktree_goes_only_once_its_candidate_is_recoverable`
- `distinct_staged_and_working_versions_keep_the_settled_worktree`
- `a_symlinked_rescue_store_keeps_the_worktree_and_writes_nothing_outside`

### `v2_worktree_lifecycle::dirty_rescue::rescue_contracts_2`

- `unpublished_head_content_recovers_after_branch_deletion_and_object_expiry`
- `an_open_task_keeps_its_dirty_source_byte_for_byte_and_loses_only_build_output`
- `a_live_or_too_recent_dirty_worktree_is_never_removed`

### `v2_worktree_lifecycle::declared_reclaim::reclaim_contracts_1`

- `sweep_reclaims_declared_paths_in_kept_worktrees_and_preserves_candidates`
- `reclaim_fails_closed_for_symlinks_tracked_content_workers_and_shared_active_paths`
- `a_reclaimed_candidate_resumes_rebuilds_with_a_fake_agent_and_completes`

### `v2_worktree_lifecycle::trash::trash_contracts_1`

- `gc_preserves_registered_dirty_trash_worktrees_in_dry_and_live_sweeps`
- `gc_preserves_recoverable_or_unverifiable_trash_git_links`

### `commit_verifier::commit_evidence_contracts_1`

- `per_task_commit_delivers_both_sides_of_a_staged_rename`
- `commit_verifier_refuses_no_diff_with_covering_commit_or_unknown_fields`
- `commit_verifier_refuses_already_landed_with_unknown_fields`
- `commit_verifier_accepts_valid_no_diff_evidence`

### `commit_verifier::commit_evidence_contracts_2`

- `commit_verifier_accepts_valid_already_landed_evidence`
- `commit_verifier_accepts_already_landed_through_symlinked_parent_workspace`
- `commit_verifier_refuses_already_landed_with_escaping_anchor_symlink`
- `commit_verifier_refusal_names_already_landed_field_contract`

### `commit_verifier::commit_evidence_contracts_3`

- `commit_verifier_keeps_refusing_unaccepted_already_landed_evidence`
- `commit_verifier_clean_tree_without_evidence_names_both_routes`
- `no_diff_expected_commits_an_unexpected_diff_and_skips_a_clean_tree`
- `clean_doc_duties_task_completes_local_and_pr_pipeline_commit_with_no_diff_evidence`

### `commit_verifier::commit_evidence_contracts_4`

- `owner_delivery_refuses_nested_repos_and_never_runs_their_filters`

### `commit_verifier::already_landed::claimed_evidence_contracts_1`

- `claimed_commit_accepts_required_checks_with_controls_and_diagnostics`
- `claimed_commit_refuses_incomplete_or_mismatched_mixed_role_evidence`

### `candidate_resume::candidate_resume_contracts_1`

- `a_validated_candidate_delivers_without_an_implementation_step`
- `a_repair_hands_the_implementer_the_candidate_trigger`
- `a_claimed_leaf_hands_its_kept_candidate_to_resume_and_the_repair_to_the_implementer`
- `a_claimed_local_leaf_resumes_its_kept_candidate`

### `claimed_candidate_paths::candidate_path_contracts_1`

- `claimed_candidate_protected_names_ignore_ascii_case_on_every_host`
- `claimed_candidate_type_changes_refuse_non_regular_files_without_widening_existing_paths`

### `claimed_pr_reuse::published_lease_contracts_1`

- `a_resumed_published_candidate_republishes_through_its_open_pull_request`
- `an_unreusable_branch_closes_the_earlier_pull_request_for_the_new_one`
- `an_advanced_adopted_branch_is_not_overwritten_after_a_base_rebase`
- `a_deleted_adopted_branch_is_not_recreated`

### `claimed_pr_reuse::published_lease_contracts_2`

- `an_unmoved_adopted_branch_is_replaced_after_a_base_rebase`
- `an_unmoved_adopted_branch_fast_forwards_under_its_published_lease`
- `a_fast_forward_does_not_recreate_an_adopted_branch_deleted_after_observation`
- `a_fast_forward_does_not_move_an_adopted_branch_rewound_after_observation`

### `completion_review::completion_review_contracts_1`

- `shipped_completion_rebases_re_reviews_and_completes_the_new_head`
- `a_repaired_conflict_after_a_re_review_re_reviews_again_and_delivers`
- `a_conflict_after_the_second_re_review_stops_delivery`
- `conflict_recovery_rounds_are_bounded_per_failure`

### `before_landing_review::landing_review_contracts_1`

- `an_approved_before_landing_review_lands_the_head_it_settled`
- `a_before_landing_reviewer_fix_is_validated_pushed_under_a_lease_and_lands`
- `a_rejected_incomplete_or_timed_out_before_landing_review_never_merges`

### `before_landing_review::landing_review_contracts_2`

- `a_review_only_run_still_reviews_the_open_pr`
- `a_dirty_rebase_after_a_before_landing_review_routes_through_re_review`
- `a_claimed_leaf_reviews_its_open_pr_and_hands_off_the_head_it_settled`

### `review_fixes::review_settlement_contracts_1`

- `a_reviewer_timeout_retains_its_output_and_stops_before_retry_or_publication`
- `accept_publishes_the_implementation_head_without_revalidation`
- `a_report_shape_defect_returns_to_the_reviewer_once_before_settlement`
- `accept_with_fixes_revalidates_the_reviewer_commit_before_publishing_it`

### `review_fixes::review_settlement_contracts_2`

- `a_failed_revalidation_rejects_without_a_second_review`
- `reject_blocks_the_task_after_final_recovery`
- `reject_supplies_fixture_log_tail_to_final_recovery`

### `provider_capacity::provider_failure_contracts_1`

- `capacity_is_typed_only_from_text_the_failed_provider_wrote`
- `a_capacity_failure_skips_recovery_and_keeps_the_candidate`
- `refusal_is_typed_only_from_text_the_failed_provider_wrote`

### `provider_capacity::provider_failure_contracts_2`

- `a_content_filter_refusal_skips_recovery_and_keeps_the_candidate`
- `a_declared_owner_route_failure_skips_recovery`
- `other_provider_failures_still_reach_recovery_and_the_post_recovery_attempt`

### `provider_capacity::provider_failure_contracts_3`

- `a_timed_out_attempt_reaches_recovery_and_the_post_recovery_attempt`
- `a_required_validation_failure_still_reaches_step_recovery_and_post_recovery_attempt`
- `a_red_base_validation_failure_dispatches_no_recovery`

### `provider_limit::provider_limit_contracts_1`

- `usage_limits_are_typed_only_from_text_the_failed_provider_wrote`
- `an_antigravity_error_result_on_exit_zero_is_a_usage_limit`
- `reported_resets_and_windows_are_read_from_the_providers_own_words`
- `a_usage_limit_skips_recovery_and_keeps_the_candidate`

### `provider_usage_window::usage_window_contracts_1`

- `a_codex_run_records_the_window_its_own_rollout_reports`
- `a_claude_stream_records_both_windows_and_projects_as_its_result_alone`
- `a_claude_rate_limit_event_marks_rejected_and_overage_windows`

### `recovery_evidence::recovery_evidence_contracts_1`

- `both_recovery_dispatches_inject_bounded_chronological_run_evidence`
- `both_recovery_dispatches_distinguish_missing_logs_from_unreadable_or_oversized_evidence`

### `reviewer_wall_clock::review_deadline_contracts_1`

- `a_reviewer_runs_under_the_hosts_deadline_above_or_below_the_activity_wall_clock`
- `a_reviewer_keeps_the_activity_wall_clock_when_the_host_does_not_bound_reviews`
- `other_activities_keep_their_own_wall_clock`

### `history_note::history_note_contracts_1`

- `a_failure_note_inlines_ordinary_errors_and_elides_bulk_with_its_retrieval_path`
- `an_interruption_note_names_its_resume_and_shares_the_cap`

### `v2_local_shell::shell_dispatch_contracts_1`

- `shipped_reference_asset_runs_git_status_in_the_workspace`
- `a_nonzero_exit_fails_the_activity_and_is_audited`
- `a_missing_executable_fails_the_activity`
- `a_timed_out_step_is_terminated_and_reported`

### `v2_local_shell::shell_dispatch_contracts_2`

- `dispatch_input_never_reaches_argv`
- `the_activity_fs_profile_reaches_sandbox_resolution`
- `a_shell_step_gets_only_the_policy_baseline_and_its_explicit_env`

### `v2_name_resolution::catalog_contracts_1`

- `name_resolution_regressions`
- `every_shipped_job_resolves_and_passes_execution_validation`
- `validate_job_checks_output_producer_guards`
- `validate_job_rejects_duplicate_step_ids_across_nested_bodies`
- `validate_job_checks_quorum_bounds`

### `handoff_landing::handoff_landing_contracts_1`

- `a_local_candidate_landing_names_its_task_before_the_branch_moves`
- `a_candidate_that_cannot_fast_forward_records_no_landing_intent`
- `a_mismatched_merged_pr_resolves_the_intent_before_stopping_later_attempts`

### `handoff_landing::handoff_landing_contracts_2`

- `only_a_conflicting_or_stale_base_stops_a_pull_request_for_repair`
- `an_owed_hosted_check_must_pass_before_the_owner_merges`

### `forge_hold::forge_hold_contracts_1`

- `a_forge_wrapper_refuses_a_non_local_origin_before_pushing`
- `an_outage_inside_the_backoff_budget_costs_waits_and_no_step_failure`
- `an_outage_past_the_budget_holds_the_run_and_a_resume_pushes_the_same_head`

### `forge_hold::forge_hold_contracts_2`

- `a_push_window_retries_past_the_budget_and_lands_the_same_head`
- `a_push_window_that_closes_holds_since_the_first_refusal`

### `pr_landing::validation_environment_contracts`

- `missing_required_validation_tool_reports_its_search_path`
- `validation_from_a_minimal_launcher_path_uses_the_login_shell_toolchain`
- `a_missing_validation_tool_is_an_environment_failure`

### `pr_landing::empty_validation_and_handoff_contracts`

- `required_validation_without_commands_is_a_no_op`
- `claimed_validation_without_commands_is_a_recorded_no_op`
- `claimed_handoff_carries_before_pr_evidence_only_for_its_candidate`
- `claimed_handoff_without_logs_is_refused_when_commands_are_required`

### `pr_landing::failure_handoff_contracts`

- `a_failure_handoff_releases_the_run_s_review_attempts_even_for_a_bundle`
- `a_reviewer_timeout_preserves_its_candidate_and_requeues_without_blocking`
- `an_external_evidence_handoff_holds_while_a_substantive_rejection_blocks`
- `an_admission_refusal_for_held_evidence_stays_held_not_escalated`

### `pr_landing::implementation_handoff_contracts`

- `an_implementer_blocker_keeps_the_dirty_candidate_without_a_pr`
- `a_provider_failure_commits_the_candidate_without_a_pr_or_a_status_write`
- `an_upgrade_refused_reviewer_holds_the_candidate_without_blocking_the_task`
- `a_child_its_parent_stopped_waiting_for_holds_its_candidate`

### `pr_landing::pr_body_and_promotion_contracts`

- `pr_promote_records_the_pull_request_page_with_its_number`
- `pr_open_appends_the_review_fixes_section_to_the_published_body`

### `pr_landing::known_red_sandbox_notices`

- `known_red_failures_cannot_waive_host_notices_or_replay_coverage`: bubblewrap, seatbelt, deferred, skip, skipping-colon

### `pr_landing::known_red_dropped_notices`

- `known_red_failures_cannot_waive_host_notices_or_replay_coverage`: skipping-space, raw-refusal, dropped-deferral, dropped-skip

### `pr_landing::known_red_replay_coverage`

- `known_red_failures_cannot_waive_host_notices_or_replay_coverage`: replay-with-notice, replay-zero-tests, replay-missing-count, replay-missing-tool, replay-executed

### `pr_landing::pull_ref_identity_refusals`

- `previous_published_head_pull_ref_lag_refuses_unverified_or_persistent_refs`: pull_ref_third, pull_ref_third_after_old, branch_missing, branch_at_previous, previous_unknown, previous_empty

### `pr_landing::pull_ref_wait_and_review_refusals`

- `previous_published_head_pull_ref_lag_refuses_unverified_or_persistent_refs`: previous_is_candidate, persistent, persistent_after_metadata, time_cap, review_mismatch, ls_remote_fails

### `pr_landing::flaky::candidate_validate_verdicts`

- `bounded_candidate_replay_judges_flakes_failures_and_coverage_on_both_routes`: candidate_validate: flake, deterministic, over_bound, base_skip

### `pr_landing::flaky::candidate_validate_coverage`

- `bounded_candidate_replay_judges_flakes_failures_and_coverage_on_both_routes`: candidate_validate: zero, partial, mismatch, skip

### `pr_landing::flaky::claim_validate_verdicts`

- `bounded_candidate_replay_judges_flakes_failures_and_coverage_on_both_routes`: claim_validate: flake, deterministic, over_bound, base_skip

### `pr_landing::flaky::claim_validate_coverage`

- `bounded_candidate_replay_judges_flakes_failures_and_coverage_on_both_routes`: claim_validate: zero, partial, mismatch, skip

### `pr_landing::claimed_known_red_equal_and_subset`

- `a_claimed_candidate_failing_only_known_red_tests_hands_off_a_report_the_owner_rechecks`: equal, subset

### `pr_landing::claimed_known_red_superset_and_disjoint`

- `a_claimed_candidate_failing_only_known_red_tests_hands_off_a_report_the_owner_rechecks`: superset, disjoint


## Reconciled PR families (ORB-15959 salvage)

These families reuse one bounded child and pristine Git seed; each named row still makes its own copied checkout, remote, host and forge state. The partitions are bounded by measured row cost, and every production Git/forge/validation operation in each row still runs.

### `pr_landing::metadata_lag_pins_and_repair`

- `previous_published_head_metadata_lag_keeps_exact_delivery_and_review_pins`
- `previous_published_head_metadata_lag_after_in_run_conflict_repair`

### `pr_landing::pull_ref_pins_and_identity`

- `previous_published_head_pull_ref_lag_settles_with_exact_merge_pin`
- `pull_ref_identity_refusals`

### `pr_landing::pr_open_branch_identity`

- `pr_open_accepts_a_fresh_stacked_base_at_the_landing_tip`
- `pr_open_refuses_a_stacked_base_already_merged_into_the_landing_branch`
- `pr_open_refuses_a_head_or_base_other_than_the_reviewed_candidate`

### `pr_landing::pr_create_retry_contracts`

- `pr_open_permanent_create_failures_never_retry_or_look_up_again`
- `pr_open_recovers_a_transient_create_and_reuses_an_ambiguously_created_pr`
- `pr_open_stops_transient_creation_at_the_attempt_ceiling_in_the_create_phase`

### `pr_landing::pr_number_and_push_transports`

- `pr_open_preserves_provider_number_representations`
- `fixture_push_guard_checks_all_effective_push_urls`
- `fixture_push_refuses_non_file_transports_without_network`

### `pr_landing::base_race_fresh_gates`

- `a_base_modified_merge_refusal_retries_the_same_candidate_on_fresh_evidence`
- `a_base_race_refresh_refuses_unmet_gates_without_another_merge`

### `pr_landing::base_race_candidate_identity`

- `a_base_race_refresh_refuses_a_changed_candidate_without_another_merge`
- `an_already_merged_refresh_reconciles_without_another_merge_request`

### `pr_landing::merge_refusal_bounds`

- `other_merge_refusals_fail_on_the_first_request`
- `repeated_base_races_stop_at_the_attempt_ceiling_within_the_wait_budget`

### `pr_landing::recovered_base_advances`

- `conflicting_base_advances_after_recovery_are_chased_until_the_bound`
- `a_recovered_completion_rebase_follows_a_clean_base_advance_and_lands`

### `pr_landing::review_checks_and_delivery`

- `a_failed_or_blocked_check_never_merges_and_the_task_stays_in_review`
- `passing_checks_merge_the_reviewed_candidate_and_record_its_delivery`

### `pr_landing::review_fix_revalidation`

- `a_before_landing_fix_lands_under_its_lease_and_an_unreviewed_head_never_merges`
- `a_failed_review_revalidation_rejects_and_preserves_both_commits`
- `a_red_base_during_pr_revalidation_holds_the_task`

### `pr_landing::review_repair_leases`

- `a_merged_lease_preserves_the_review_repair_and_any_other_move_loses_the_lease`
- `a_conflicting_reviewed_pr_is_rebased_for_re_review_then_completes`

### `pr_landing::claimed_validation_failures`

- `a_claimed_candidate_on_a_red_base_fails_typed_with_both_logs`
- `a_claimed_candidate_that_cannot_reach_the_network_fails_transient`
- `a_required_command_red_on_the_base_holds_the_task_without_a_pr`

### `pr_landing::claimed_publication_and_merge_pins`

- `a_pr_claim_validates_before_publication_and_pins_after_it`
- `a_pr_merge_batch_lands_the_checked_head_at_its_pin`
- `a_pr_merge_batch_refuses_a_head_that_moved_after_its_freshness_check`

### `pr_landing::validation_repairs_and_logs`

- `required_validation_refuses_a_failing_candidate_until_a_committed_repair_passes`
- `required_validation_bounds_diagnostics_and_retains_candidate_and_base_logs`
- `printed_supervisor_notes_remain_candidate_failures_on_both_streams`

### `pr_landing::supervisor_and_truncated_failures`

- `supervisor_stops_are_environment_failures_and_never_poison_the_base_cache`
- `a_failure_only_the_truncated_middle_names_still_holds_the_candidate`

### `pr_landing::network_retry_and_cached_base`

- `a_network_flake_is_rerun_before_the_candidate_is_judged`
- `remote_base_transport_retries_preserve_access_refusals`
- `claimed_handoff_fetch_timeout_uses_only_a_related_cached_base`

### `pr_landing::linux_deferral_contracts`

- `a_claimed_leaf_replays_its_implementers_deferral_before_publication`
- `a_deferral_that_is_not_the_sanctioned_case_fails_the_implement_step`
- `a_documented_bubblewrap_deferral_is_accepted_and_replayed_before_delivery`

### `pr_landing::macos_deferral_contracts`

- `a_documented_seatbelt_deferral_is_accepted_and_replayed_before_publication`
- `a_macos_claimed_leafs_ps_denied_deferral_is_replayed_before_publication`
- `a_seatbelt_and_ps_denied_deferral_is_replayed_like_a_single_sandboxs`
- `a_required_pass_that_deferred_a_sandbox_path_is_an_environment_failure`

### `pr_landing::validation_and_empty_handoffs`

- `validation_environment_contracts`
- `empty_validation_and_handoff_contracts`

### `pr_landing::failure_and_implementation_handoffs`

- `failure_handoff_contracts`
- `implementation_handoff_contracts`

### `pr_landing::known_red_validation_and_subset`

- `a_candidate_failing_only_known_red_tests_proceeds_and_records_them`
- `claimed_known_red_equal_and_subset`

## Admission audit families

Each named row keeps a fresh disposable home, temporary directory, cwd, authority marker, host and state. Only the bounded child executable is shared. Environment and cwd restore before the next row, including on unwind.

### `admission_audit::commit::candidate_paths::candidate_path_authority_contracts`

- `scratch_under_orbit_tmp_is_never_delivered`
- `claimed_new_paths_outside_the_footprint_deliver_and_request_owner_widening`
- `owner_delivery_commits_out_of_selector_paths_and_widens_with_provenance`
- `per_task_bundles_attribute_each_path_to_the_task_whose_agent_changed_it`
- `owner_new_path_validation_refuses_symlinks_and_widens_rename_destinations`

### `admission_audit::gc::classify::gc_ownership_contracts`

- `gc_keeps_an_unregistered_directory_whose_git_link_still_resolves`
- `gc_keeps_a_leftover_whose_task_is_not_settled`

### `admission_audit::gc::leftovers::gc_leftover_lifetime_contracts`

- `gc_reclaims_the_leftover_of_a_removal_that_failed_partway`
- `gc_reclaims_trash_directory_when_deleter_pid_is_dead`
- `gc_reclaims_trash_directory_when_age_exceeds_timeout`
- `gc_keeps_trash_directory_when_deleter_pid_is_alive_and_within_timeout`
- `gc_leaves_malformed_trash_directory_as_unrecognized`

### `admission_audit::gc::sweep::gc_symlink_and_read_only_contracts`

- `symlink_at_a_known_worktree_path_is_never_followed`
- `gc_removes_a_worktree_holding_a_read_only_directory`

### `admission_audit::trusted_host::trusted_host_authority_contracts`

- `a_forged_admission_value_does_not_admit_the_mode`
- `an_admission_alone_does_not_remove_an_ordinary_activitys_sandbox`
- `distinct_target_keeps_the_pipeline_step_id_in_trusted_host_audit`

## QA required selector reconciliation

The workflow-process-boundary inventory now requires the surviving families above. Its two old required selectors `v2_runtime::deterministic_reference_dispatches_and_persists_audit_events` and `v2_runtime::parallel_join_policy_decides_the_block_from_its_branches` both execute in `v2_runtime::runtime_contracts_1`; the inventory requires that family once because duplicate required selectors are invalid. Both behavioral rows and their assertions remain required inside that family. The other eight old required selectors each map to the family listed above. No surface, behavior class or assertion entry was removed.
