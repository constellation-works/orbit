---
type: review
summary: MCP and task CLI test-budget measurements and coverage mapping
last_validated: 2026-10-13
---
# MCP and task CLI test budget

The suites retain production stdio, tool discovery, schema snapshots, CLI
argument/output contracts, refusal atomicity, authority isolation and process
handover coverage. Named rows keep their assertions and platform gates. The
shared workspace Git-decoy regression tests both hostile locator sets and
unchanged sentinel refs; upstream retired the per-binary containment wrappers.
Recovery tables re-execute one exact registered table in an isolated child.

Task tables reuse a workspace only where assertions address independent task
IDs. The accepted version-one receipt and config commit-lock checks reuse
existing equivalent recovery/review state instead of building another fixture.
The terminal-state matrix covers every terminal status and both checkpoint
representations without their redundant Cartesian product.

Nextest bounds task fixtures to four slots and MCP fixtures to eight slots.
These limits apply to the two complete integration binaries, including future
rows, and leave other binaries in their existing groups. They bound overlapping
CLI/database startup on shared hosts; they are scheduling policy, not assertions
about behavior. Cargo's ordinary test runner still executes the same tables.

## Candidate-host measurements

| Binary | Before tests | After tests | Before summed s | After summed s | Reduction | Before wall s | After wall s |
|---|---:|---:|---:|---:|---:|---:|---:|
| mcp | 136 | 58 | 465.335 | 188.281 | 59.54% | 53.875 | 62.034 |
| task | 133 | 61 | 397.944 | 140.670 | 64.65% | 39.821 | 35.904 |

Measurements use sequential `cargo nextest run -p orbit-cli --test mcp` and
`cargo nextest run -p orbit-cli --test task` invocations on the same macOS host.
A tool config adds JUnit output, and success output is captured so deferrals
cannot disappear. Summed time is the sum of executed JUnit testcase durations;
wall time is nextest's test-run summary, excluding compilation. The base is
7f94c07633f3b9c94c1282b2a82ff8022d2ec4f3. The baseline ran before edits in the
same checkout; rebuilt candidate test lists prove the new source was exercised.
These measurements precede conflict recovery onto the newer integration base,
which already removed the MCP and task containment wrappers. Recovery keeps
those removals and the shared workspace Git-decoy regression, including
upstream's removal of the task suite's ignored Git-decoy child entry.

The original Linux reference was 140 MCP tests / 192 seconds and 132 task tests /
182 seconds. Those absolute durations are not directly comparable to this Mac;
the acceptance ratio uses each binary's fresh baseline on the candidate host.
Shared-host wall time and summed duration can vary independently. The report
states both; a lower summed time is not a claim of equal wall-time improvement.

### Drain adoption coverage repair

One isolated process row restores drain executable handover without restoring
the retired live-process matrices. It uses the production-built executable,
a 300-second drain window and one-second admission cadence; successful runs
cancel the adopted owner immediately, and an unwind guard cancels it on failure.
Locked participant records prove the executable digest changed. The same run,
PID and start time survive, and a later admission pass proves the new image
continued execution before cancellation verifies its exit and terminal state.
Candidate admission and MCP session continuity do not cover this run adoption.

Sequential, build-budget-admitted Linux runs on base
e705d95af261f1b78eab9f24717512575c8f8e5a and the repair measured:

| MCP suite | Tests | Summed s | Test wall s |
|---|---:|---:|---:|
| Before repair | 60 | 119.888 | 33.783 |
| After repair | 61 | 122.776 | 33.904 |

The restored row took 3.225 seconds in nextest (3.15 seconds in the focused
Cargo run). Summed time here adds nextest's per-test durations; wall time excludes
compilation and build admission. Both runs deferred the same two existing
Bubblewrap read-only fixtures because the host refused user namespaces; the
drain row ran natively without deferral. These shared-host observations preserve
the broader reduction and do not establish a speedup. A temporary control
changed only the adoption environment key passed to `reexec`: the focused row
failed with `drain exited before adoption` in 2.95 seconds. The production source
was restored byte-for-byte before the candidate suite ran. macOS execution is
not measured here; its existing `mcp_roundtrip::upgrade::` CI filter includes
the restored row and its native candidate assessment/signing helpers.

### Upgrade fixture staging cost

The three native upgrade identities retain their original rows and isolated
workspaces. The manual drain, invalid-request and oversized-partial-request
replacements now rename the candidate already prepared by `distinct_copy` over
the installation. This retains a distinct assessed executable, atomic replacement
of the path while the old inode runs, and real process re-execution. It removes
three second copies of the 376,159,086-byte candidate and three second assessment
commands in the measured selection. The local-candidate update row still invokes
the production installer, including its own staging, probe and manifest checks.
The drain cleanup guard stays armed until cancellation, owner exit and terminal
state are all asserted, then avoids cancelling that completed run again.

The table emits each row's identity before execution and its elapsed time after
completion. Its review-schema row still bootstraps the feature migration through
`task list`: an attempted removal failed with `no such table:
feature_schema_meta` and was reverted. No behavioral row or assertion was retired.

On Linux `dk-server-2`, source base
`3e7135562c01af7903f7f8db65cb1fcd73fb7cdf` and the uncommitted candidate measured:

| Native nextest identity (`mcp_roundtrip::upgrade::`) | Baseline s | Candidate s |
|---|---:|---:|
| `a_replaced_drain_hands_its_run_to_the_installed_executable` | 4.426 | 2.822 |
| `a_replaced_mcp_server_hands_its_session_over_after_invalid_requests` | 3.392 | 3.253 |
| `upgrade_admission_and_session_handover` | 12.196 | 11.355 |
| Summed test time | 20.014 | 17.430 |
| Nextest test wall time | 12.197 | 11.356 |

Both arms selected and executed the same three identities, with 58 other entries
excluded by the filter. They used the host's existing eight build slots, four
Cargo jobs and four test threads, the unchanged MCP group and default nextest
profile, and disabled compiler caches. Load averages before/after were
10.928/10.566/11.325 → 6.029/8.780/10.516 for the baseline and
6.118/6.921/9.353 → 7.731/7.237/9.430 for the candidate on 32 CPUs. These are
single shared-host samples, not a statistical speedup claim. End-to-end commands
took 177.542s (cold baseline build) and 14.601s (incremental candidate build);
those build costs are not comparable throughput measurements.

The complete MCP and process targets subsequently ran 135 tests in 52.758s of
test wall time (207.887s summed; 57.391s including compilation). The three named
identities took 2.801s, 3.212s and 11.505s. The separately repaired flow callback
passed in 0.547s; the dashboard staging helper's unchanged behavior passed in
5.674s. Six entries were excluded by platform/configuration, and two existing
read-only Bubblewrap scenarios reported namespace-permission deferrals; those
confined paths were not executed. A selector-listing check was briefly started
alongside this run and stopped to restore sequential Cargo validation, so this
run is coverage evidence rather than a performance comparison.

The retained parent macOS output reports 28.912s, 30.686s and 50.942s at source
`13945db3a357d2c3f056c34214b32717af864d99`. That source object and its exact build
budget were unavailable here; this Linux pair does not establish a macOS repair.
The current checkout still uses the five-minute nextest timeout, so the
15-second target is assessed from recorded durations. The four required delivery
gates run separately in pipeline final validation; these focused measurements
do not establish the complete CI job's 15-minute budget. Raw outputs, JUnit,
source snapshots, diagnostic trace and all failed attempts are retained as task
runtime artifacts.

## Retired tests

Each line identifies a removed entry and its surviving coverage or redundancy
reason. Tables in the next section retain the bodies of consolidated entries.

### [crates/orbit-cli/tests/mcp/mcp_roundtrip.rs](../../crates/orbit-cli/tests/mcp/mcp_roundtrip.rs)

- mcp_friction_rehome_moves_a_record_into_its_registered_owner: task::friction_lifecycle_cli::friction_cli_triage_stats_and_rehome_preserve_workspace_ownership already checks the same ownership transfer through the CLI; MCP record mutation remains in mcp_serve_round_trips_records_against_a_temp_workspace.
- mcp_friction_refused_rehome_leaves_the_edits_unapplied: The same refusal atomicity is checked by task::friction_lifecycle_cli::friction_cli_triage_stats_and_rehome_preserve_workspace_ownership; generic MCP structured errors remain covered.
- mcp_search_without_query_or_tag_keeps_its_refusal_message: tool_input_hints::mcp_input_misses_return_actionable_suggestions and mcp_serve_error_paths_return_tool_errors_and_keep_serving cover malformed input and the live transport.
- a_replica_checkout_refuses_a_coordination_write_with_capability_refused: a_replica_mcp_session_enforces_checkout_capability_classes covers the same coordination-write gate alongside its positive controls.
- a_replica_mcp_session_closes_legacy_local_frictions: task::friction_lifecycle_cli::replica_closes_legacy_local_frictions_with_audit_without_owner_mutations covers the domain transition; a_replica_mcp_session_enforces_checkout_capability_classes covers MCP replica admission.
- workspace_init_mcp_config_reaches_a_governed_tool_over_the_real_transport: fresh_workspace_init_mcp_explains_local_ship_and_allows_pr_worktree_setup launches the generated operator config over stdio and exercises a governed workflow.
- task_artifact_get_resolves_globally_outside_any_checkout: task_artifact_get_is_global_by_default_across_tool_run_task_cli_and_mcp includes the unbound launch and global-ID read.
- mcp_task_artifact_get_follows_the_global_id_and_explicit_workspace_stays_a_filter: task_artifact_get_is_global_by_default_across_tool_run_task_cli_and_mcp includes cross-workspace reads and explicit-filter refusal.
- mcp_task_eligible_is_advertised_read_only_and_answers_over_stdio: task::task_eligible::eligible_lists_candidates_clear_of_in_flight_locks_and_explains_the_rest checks eligibility; the production tools/list snapshot pins the MCP schema and annotations, and record reads remain exercised over stdio.
- mcp_task_add_and_update_validate_context_selectors: task::task_trimmed_surface context-selector CLI contracts and context_selector_worktree cover validation and unchanged-on-refusal; MCP add/update and invalid_input remain in the record and error rows.
- mcp_task_writes_report_readiness_gaps: task::task_trimmed_surface::task_show_and_write_tools_report_readiness_gaps covers the shared tool response; MCP task writes remain in the record row.
- managed_cli_from_linked_worktree_updates_canonical_workspace: task::context_selector_worktree exercises owning-worker CLI/tool writes from the linked checkout; managed_mcp_config_routes_a_linked_worktree_by_its_logical_workspace_id retains the MCP routing proof.
- mcp_task_show_follows_the_global_id_and_explicit_workspace_stays_a_filter: task_show_is_global_by_default_across_tool_run_and_mcp includes the same global lookup and fail-closed explicit filter.

### [crates/orbit-cli/tests/mcp/mcp_setup_root.rs](../../crates/orbit-cli/tests/mcp/mcp_setup_root.rs)

- external_root_task_index_loss_is_reported_and_reindexed: task::task_registry_recovery::lost_registry_refuses_allocation_until_the_high_water_mark_is_reestablished exercises actual registry loss/reindex and non-reissued IDs; external-root MCP init/remove remains covered.

### [crates/orbit-cli/tests/mcp/mcp_roundtrip/upgrade.rs](../../crates/orbit-cli/tests/mcp/mcp_roundtrip/upgrade.rs)

- a_newer_build_writes_while_older_mcp_dashboard_and_drain_processes_stay_live: generation_admission::a_data_only_feature_migration_joins_beside_an_older_live_participant covers compatible live generations; persistent_client_hands_over_to_a_local_candidate_update retains production MCP handover. Dashboard/drain are unrelated to the MCP wire contract.
- a_replaced_drain_hands_its_run_to_the_installed_executable: Restored here as one isolated process-boundary row covering `drain_upgrade_boundary`, `reexec` and run adoption through `ORBIT_ADOPT_PIPELINE_RUN`, followed by cancellation of the adopted owner. generation_admission::a_candidate_admission_admits_beside_a_server_that_hands_over_to_it covers candidate admission, and persistent_client_hands_over_to_a_local_candidate_update covers MCP session handover; neither replaces this drain contract.
- a_pending_breaking_switch_waits_for_live_processes_to_yield_at_safe_points: generation_admission::a_breaking_upgrade_refuses_ordinary_joins_until_live_participants_yield covers the shared switch gate; a_replaced_mcp_server_defers_handover_until_a_large_partial_request_completes retains the MCP safe-point boundary.
- read_only_candidate_joins_a_v1_generation_without_rewriting_the_record: generation_admission::concurrent_readers_of_a_v1_record_read_the_store_schema_in_parallel exercises v1 read-only admission at the owning public API.
- read_only_foreign_digest_refuses_when_store_schema_differs: generation_admission tests cover schema compatibility and fail-closed live admission at the owning public API; new_review_record_contract_refuses_a_live_old_client_before_mutation retains a production incompatible-client proof.
- a_local_candidate_refuses_live_clients_untouched_and_serves_them_once_they_reconnect: new_review_record_contract_refuses_a_live_old_client_before_mutation retains incompatible-client refusal; persistent_client_hands_over_to_a_local_candidate_update and the partial/invalid-request handover rows retain production session continuity.

### [crates/orbit-cli/tests/mcp/mcp_roundtrip/desktop.rs](../../crates/orbit-cli/tests/mcp/mcp_roundtrip/desktop.rs)

- desktop_task_status_sets_compose_with_search_priority_and_pagination: task::task_tags::search_status_modes_include_blocked_and_deferred_tasks and task_list CLI contracts exercise the shared status filters; desktop_writes_reconcile_after_restart_and_reject_stale_or_implicit_destinations retains the desktop boundary.

### [crates/orbit-cli/tests/task/task_trimmed_surface.rs](../../crates/orbit-cli/tests/task/task_trimmed_surface.rs)

- task_update_complexity_roundtrips_through_a_real_task_record: A typed field round trip repeats the add/update/show path; task_update_repeats_dependencies_and_clears_context and task_cli_roundtrips_filters_and_replaces_tags retain repeated/cleared CLI argument contracts.
- task_add_rejects_missing_context_file: task_update_rejects_missing_context_file_and_leaves_task_unchanged and selector_missing_from_both_checkouts_is_rejected_from_a_worktree retain CLI context rejection and atomicity; add/update share the selector validator.
- lint_sweep_keeps_declared_context_when_its_target_disappears: Core runtime task_lint_paths and task_pilot scope admission cover path validation; allow_missing_context_records_durable_exact_creation_intent retains the CLI declaration/creation-grant contract.
- lint_reports_an_empty_declaration_and_restores_no_guessed_scope: Core runtime task_lint_paths covers domain path findings; task_show_and_write_tools_report_readiness_gaps retains CLI output for missing scope.
- lint_reads_a_path_closed_by_a_parenthesis_and_full_stop_as_the_file: orbit-core::runtime::task_lint_paths::lint_reads_only_repository_paths_as_path_mentions checks the parser at its owning API, without repeating workspace startup through the CLI.
- locks_reserve_requires_the_same_operator_capability_as_release: locks_release_reaches_the_admin_tool_only_with_the_operator_capability retains CLI operator dispatch; reservation_cli_conflicts_and_confirmation_preserve_claims retains real reserve refusal and persistence.
- run_cancel_confirmation_precedes_run_lookup: Confirmation ordering repeats the governed CLI confirmation path retained by reservation_cli_conflicts_and_confirmation_preserve_claims; workflow cancel is not a task CLI contract.
- migrate_bare_invocation_inspects_and_confirm_applies: Workspace migration is outside the task contract; workspace initialization/recovery is exercised by the workspace integration binary.
- workspace_remove_is_recoverable_by_reinitializing_the_checkout: Workspace lifecycle is covered in the workspace integration binary and the external-root MCP init/remove contract; this task retains task-specific archive/import recovery.

### [crates/orbit-cli/tests/task/audit_cli.rs](../../crates/orbit-cli/tests/task/audit_cli.rs)

- audit_cli_fixture_keeps_isolated_home_untouched_by_log_events: audit_cli_round_trips_real_mutation_filters_stats_and_export retains isolated-home command auditing; the removed row rechecks fixture setup rather than another CLI contract.
- audit_stats_policy_count_matches_dashboard_fixture: Dashboard policy-count formatting repeats the audit stats aggregation checked by audit_cli_round_trips_real_mutation_filters_stats_and_export.
- success_via_runtime_yields_exactly_one_row: Direct RuntimeHost dispatch/audit is core behavior, already exercised by orbit-core runtime admission_audit boundary tests; audit_cli_round_trips_real_mutation_filters_stats_and_export retains actual CLI exactly-once auditing.

### [crates/orbit-cli/tests/task/task_list.rs](../../crates/orbit-cli/tests/task/task_list.rs)

- task_list_help_documents_rule: Help goldens already pin the CLI surface; this row only matches prose and conflicts with AGENTS.md behavioral-test guidance.
- existing_filters_keep_behaviour: task_cli_roundtrips_filters_and_replaces_tags and search_status_modes_include_blocked_and_deferred_tasks retain actual CLI tag/status filtering.

### [crates/orbit-cli/tests/task/auto_task_settings_cli.rs](../../crates/orbit-cli/tests/task/auto_task_settings_cli.rs)

- every_shipped_body_history_source_is_recognized_by_workspace_sync: older_shipped_bodies_upgrade_with_settings_and_custom_bodies_stay_forked retains the migration/fork distinction; enumerating every historical prose revision repeats the same classifier and sync path.
- older_shipped_body_keeps_explicit_settings_and_their_edit_stamp: bundled_auto_task_settings_stay_outside_the_body_and_survive_a_body_refresh retains explicit-settings persistence and body refresh; older_shipped_bodies_upgrade_with_settings_and_custom_bodies_stay_forked retains old-body migration.
- older_shipped_probe_bodies_recognize_rendered_and_historical_base_branches: older_shipped_bodies_upgrade_with_settings_and_custom_bodies_stay_forked retains shipped-body versus operator-fork recognition; historical branch strings repeat the same managed-body normalization path.

### [crates/orbit-cli/tests/task/claimed_delivery_recovery.rs](../../crates/orbit-cli/tests/task/claimed_delivery_recovery.rs)

- terminal_claim_adopts_settings_and_doctor_reports_its_status / false archived, true rejected, false done: Status/checkpoint cross-products repeat terminal adoption: retained rows cover every terminal status and both checkpoint representations; archived reissue and scheduler retain both representations too.
- unminted_retry_recovers_after_a_legacy_task_closed_and_settings_changed / rejected, done: These statuses repeat the archived terminal-predecessor path; terminal adoption retains rejected/done, while unminted retry retains both resolvable and deleted predecessors.

### [crates/orbit-cli/tests/mcp/main.rs](../../crates/orbit-cli/tests/mcp/main.rs)

- mcp_git_fixtures_preserve_inherited_authority_decoy / mcp_roundtrip::fresh_workspace_init_mcp_explains_local_ship_and_allows_pr_worktree_setup: Repeats the same git_repo::command authority scrub. The shared workspace git_fixture_isolation::git_fixtures_preserve_inherited_authority_decoy regression retains both hostile locator sets and decoy-state checks; upstream removed the per-binary wrapper.
- mcp_git_fixtures_preserve_inherited_authority_decoy / mcp_roundtrip::managed_mcp_config_routes_a_linked_worktree_by_its_logical_workspace_id: Repeats the same git_repo::command authority scrub. The shared workspace git_fixture_isolation::git_fixtures_preserve_inherited_authority_decoy regression retains both hostile locator sets and decoy-state checks; upstream removed the per-binary wrapper.

- mcp_git_fixtures_preserve_inherited_authority_decoy / mcp_setup_root::mcp_init_binds_an_external_root_checkout_and_remove_reverses_it: Upstream removed the remaining wrapper row. The setup boundary fixture remains; the shared workspace Git-decoy regression owns containment.

### [crates/orbit-cli/tests/task/main.rs](../../crates/orbit-cli/tests/task/main.rs)

- task_git_fixtures_preserve_inherited_authority_decoy / auto_task_lifecycle_cli::fixture_git_commits_and_refs_stay_in_the_fixture: Upstream removed the remaining wrapper row. The shared workspace Git-decoy regression retains fixture Git operations and owns containment.
- task_git_fixtures_preserve_inherited_authority_decoy / task_publication::operator_workflow_is_network_free_labelled_and_fail_closed: Repeats the same git_repo::command authority scrub. The shared workspace git_fixture_isolation::git_fixtures_preserve_inherited_authority_decoy regression retains both hostile locator sets and decoy-state checks; upstream removed the per-binary wrapper.
- task_git_fixtures_preserve_inherited_authority_decoy / shared_root_task_isolation::shared_explicit_root_keeps_task_bundles_isolated_by_selected_workspace: Repeats the same git_repo::command authority scrub. The shared workspace git_fixture_isolation::git_fixtures_preserve_inherited_authority_decoy regression retains both hostile locator sets and decoy-state checks; upstream removed the per-binary wrapper.
- task_git_fixtures_preserve_inherited_authority_decoy / context_selector_external_root::linked_worktree_of_an_external_root_checkout_can_declare_its_own_file: Repeats the same git_repo::command authority scrub. The shared workspace git_fixture_isolation::git_fixtures_preserve_inherited_authority_decoy regression retains both hostile locator sets and decoy-state checks; upstream removed the per-binary wrapper.
- task_git_fixtures_preserve_inherited_authority_decoy / context_selector_worktree::linked_worktree_caller_can_declare_a_file_that_exists_only_there: Repeats the same git_repo::command authority scrub. The shared workspace git_fixture_isolation::git_fixtures_preserve_inherited_authority_decoy regression retains both hostile locator sets and decoy-state checks; upstream removed the per-binary wrapper.
- task_git_fixtures_preserve_inherited_authority_decoy / auto_task_lifecycle_cli::seeded_auto_task_defaults_are_inert_portable_and_name_only_callable_tools: Repeats the same git_repo::command authority scrub. The shared workspace git_fixture_isolation::git_fixtures_preserve_inherited_authority_decoy regression retains both hostile locator sets and decoy-state checks; upstream removed the per-binary wrapper.
- task_git_fixtures_preserve_inherited_authority_decoy / audit_cli::audit_cli_round_trips_real_mutation_filters_stats_and_export: Repeats the same git_repo::command authority scrub. The shared workspace git_fixture_isolation::git_fixtures_preserve_inherited_authority_decoy regression retains both hostile locator sets and decoy-state checks; upstream removed the per-binary wrapper.

## Consolidated coverage

Each original entry remains a named row or is folded into an equivalent
already-initialized fixture with its assertions intact.

### [crates/orbit-cli/tests/mcp/mcp_roundtrip.rs](../../crates/orbit-cli/tests/mcp/mcp_roundtrip.rs)

- mcp_server_advertises_governed_tools_but_denies_an_unprivileged_session → governed_session_admission. Named table row; original body and assertions retained.
- a_replica_mcp_session_enforces_checkout_capability_classes → governed_session_admission. Named table row; original body and assertions retained.
- an_operator_served_mcp_session_reaches_a_governed_tool → governed_session_admission. Named table row; original body and assertions retained.
- a_remote_originated_operator_session_reaches_a_governed_tool → forwarded_caller_authorization. Named table row; original body and assertions retained.
- a_remote_originated_agent_session_is_refused_a_governed_tool → forwarded_caller_authorization. Named table row; original body and assertions retained.
- the_forwarded_caller_label_reaches_the_destinations_authorization_audit_row → forwarded_caller_authorization. Named table row; original body and assertions retained.
- leftover_caller_authorization_files_are_ignored_rather_than_enforced → agent_invocation_envelopes. Named table row; original body and assertions retained.
- a_remote_operator_session_invokes_an_agent_end_to_end → agent_invocation_envelopes. Named table row; original body and assertions retained.
- an_invocation_without_a_response_envelope_fails_with_its_reason → agent_invocation_envelopes. Named table row; original body and assertions retained.
- a_remote_agent_session_cannot_invoke_an_agent → session_authority_and_unbound_discovery. Named table row; original body and assertions retained.
- a_local_session_keeps_its_argv_authority → session_authority_and_unbound_discovery. Named table row; original body and assertions retained.
- mcp_serve_lists_the_canonical_surface_outside_any_checkout → session_authority_and_unbound_discovery. Named table row; original body and assertions retained.
- task_artifact_get_is_global_by_default_across_tool_run_task_cli_and_mcp → global_artifacts_and_workspace_selectors. Named table row; original body and assertions retained.
- every_workspace_scoped_tool_behavior_matches_its_advertised_selector_requirement → global_artifacts_and_workspace_selectors. Named table row; original body and assertions retained.
- uninitialized_unbound_mcp_launch_gives_setup_guidance_without_operator_authority → global_artifacts_and_workspace_selectors. Named table row; original body and assertions retained.
- ssh_marked_mcp_server_audits_caller_and_server_identity_separately → transport_peer_identity. Named table row; original body and assertions retained.
- mcp_listen_round_trips_over_a_loopback_socket_and_audits_the_peer_ip → transport_peer_identity. Named table row; original body and assertions retained.
- mcp_listen_workspace_flag_binds_the_sessions_it_accepts → transport_peer_identity. Named table row; original body and assertions retained.
- mcp_serve_round_trips_records_against_a_temp_workspace → record_mutation_and_error_protocol. Named table row; original body and assertions retained.
- mcp_serve_error_paths_return_tool_errors_and_keep_serving → record_mutation_and_error_protocol. Named table row; original body and assertions retained.
- mcp_workspace_argument_must_be_a_string_and_a_blank_one_defers_to_the_session → record_mutation_and_error_protocol. Named table row; original body and assertions retained.
- mcp_task_list_fields_projects_each_task_to_an_object_of_just_those_fields → task_projection_search_and_audit. Named table row; original body and assertions retained.
- task_mutations_are_immediately_searchable_from_the_cli_and_mcp → task_projection_search_and_audit. Named table row; original body and assertions retained.
- mcp_calls_are_audited_once_including_unknown_raw_names → task_projection_search_and_audit. Named table row; original body and assertions retained.
- worktree_backed_activity_routes_task_and_search_by_advertised_workspace_argument → worktree_and_federated_routing. Named table row; original body and assertions retained.
- managed_mcp_config_updates_a_task_without_a_workspace_argument → worktree_and_federated_routing. Named table row; original body and assertions retained.
- federated_mcp_serve_requires_the_machine_qualified_list_selector → worktree_and_federated_routing. Named table row; original body and assertions retained.
- federated_mcp_serve_lists_and_routes_local_workspaces_without_destinations → federated_local_destinations. Named table row; original body and assertions retained.
- direct_and_federated_local_calls_record_equivalent_audit_contexts → federated_local_destinations. Named table row; original body and assertions retained.
- federated_mcp_serve_collapses_an_explicit_local_destination_row → federated_local_destinations. Named table row; original body and assertions retained.
- federated_mcp_serve_lists_local_workspaces_beside_unreachable_remotes → federated_host_configuration. Named table row; original body and assertions retained.
- federated_mcp_serve_reads_the_host_file_and_refuses_both_host_files → federated_host_configuration. Named table row; original body and assertions retained.
- federated_mcp_serve_rejects_a_machine_id_only_destination_row → federated_host_configuration. Named table row; original body and assertions retained.
- managed_mcp_env_binding_updates_without_a_workspace_argument → managed_environment_binding. Named table row; original body and assertions retained.
- managed_codex_mcp_env_enforces_activity_tools_and_agent_actor_kind → managed_environment_binding. Named table row; original body and assertions retained.
- unmanaged_orbit_workspace_env_does_not_bind_mcp → managed_environment_binding. Named table row; original body and assertions retained.
- managed_source_inspection_mcp_search_uses_the_dispatching_workspace → global_task_reads_and_source_inspection. Named table row; original body and assertions retained.
- task_show_is_global_by_default_across_tool_run_and_mcp → global_task_reads_and_source_inspection. Named table row; original body and assertions retained.
- task_read_surfaces_tolerate_a_crew_this_host_does_not_define → global_task_reads_and_source_inspection. Named table row; original body and assertions retained.
- workspace_tool_calls_reuse_one_runtime_until_the_registry_changes → live_runtime_and_app_presentation. Named table row; original body and assertions retained.
- workspace_tool_calls_observe_a_config_toml_edit_made_during_the_session → live_runtime_and_app_presentation. Named table row; original body and assertions retained.
- mcp_apps_presentation_reads_explicit_tasks_without_authority_or_workspace_fallback → live_runtime_and_app_presentation. Named table row; original body and assertions retained.
- absent_unwritable_partition_keeps_cli_tool_reads_observational → observational_read_only_stores. Named table row; original body and assertions retained.
- read_only_registry_files_keep_uncheckpointed_wal_reads_observational → observational_read_only_stores. Named table row; original body and assertions retained.
- absent_global_semantic_index_keeps_read_only_cli_reads_observational → observational_read_only_stores. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/mcp/mcp_setup_root.rs](../../crates/orbit-cli/tests/mcp/mcp_setup_root.rs)

- mcp_setup_expands_tilde_and_anchors_relative_roots_for_flags_and_environment → setup_root_expansion_and_config_confinement. Named table row; original body and assertions retained.
- outside_client_config_guard_rejects_config_files_in_scanned_directories → setup_root_expansion_and_config_confinement. Named table row; original body and assertions retained.
- federated_home_setup_works_outside_a_workspace_and_reports_no_binding → setup_root_expansion_and_config_confinement. Named table row; original body and assertions retained.
- federated_setup_inside_a_registered_checkout_reports_no_binding → federated_and_external_root_resolution. Named table row; original body and assertions retained.
- mcp_init_resolves_the_registered_checkout_from_an_unrelated_directory → federated_and_external_root_resolution. Named table row; original body and assertions retained.
- orbit_root_env_selects_the_same_checkout_as_the_root_flag → federated_and_external_root_resolution. Named table row; original body and assertions retained.
- explicit_root_outranks_a_different_registered_cwd_checkout → shared_and_explicit_root_precedence. Named table row; original body and assertions retained.
- shared_root_mcp_setup_uses_cwd_checkout_for_init_and_remove → shared_and_explicit_root_precedence. Named table row; original body and assertions retained.
- a_root_without_a_registered_checkout_refuses_instead_of_writing → shared_and_explicit_root_precedence. Named table row; original body and assertions retained.
- an_unregistered_root_reports_the_root_and_registered_candidates → missing_root_guidance_and_empty_config_removal. Named table row; original body and assertions retained.
- toml_client_home_config_without_other_entries_is_deleted_when_removal_empties_it → missing_root_guidance_and_empty_config_removal. Named table row; original body and assertions retained.
- json_client_home_config_symlink_survives_init_and_remove → symlinked_client_configuration. Named table row; original body and assertions retained.
- toml_client_home_config_symlink_survives_init_and_remove → symlinked_client_configuration. Named table row; original body and assertions retained.
- json_client_home_config_symlink_is_emptied_when_removal_leaves_no_entries → symlinked_client_configuration. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/mcp/mcp_roundtrip/upgrade.rs](../../crates/orbit-cli/tests/mcp/mcp_roundtrip/upgrade.rs)

- new_review_record_contract_refuses_a_live_old_client_before_mutation → upgrade_admission_and_session_handover. Named table row; original body and assertions retained.
- persistent_client_hands_over_to_a_local_candidate_update → upgrade_admission_and_session_handover. Named table row; original body and assertions retained.
- a_replaced_mcp_server_defers_handover_until_a_large_partial_request_completes → upgrade_admission_and_session_handover. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/mcp/mcp_roundtrip/plugins.rs](../../crates/orbit-cli/tests/mcp/mcp_roundtrip/plugins.rs)

- brokered_plugin_calls_audit_only_caller_owned_failures → plugin_failure_auditing. Named table row; original body and assertions retained.
- managed_global_plugin_tool_refusals_are_audited → plugin_failure_auditing. Named table row; original body and assertions retained.
- unreachable_host_read_is_audited_once_by_cli → plugin_failure_auditing. Named table row; original body and assertions retained.
- unbrokered_plugin_refusals_are_audited_once_over_mcp_and_cli → plugin_errors_and_discovery. Named table row; original body and assertions retained.
- exec_plugin_error_reaches_mcp_caller_as_structured_content → plugin_errors_and_discovery. Named table row; original body and assertions retained.
- an_enabled_plugin_tool_is_advertised_and_callable_and_a_disabled_one_is_not → plugin_errors_and_discovery. Named table row; original body and assertions retained.
- a_live_session_refreshes_plugin_tools_and_reuses_an_unchanged_surface → live_plugin_configuration. Named table row; original body and assertions retained.
- a_live_session_refreshes_definition_admission_and_membership → live_plugin_configuration. Named table row; original body and assertions retained.
- a_workspace_toggle_reaches_a_live_bound_session_and_global_tools_follow_the_host → live_plugin_configuration. Named table row; original body and assertions retained.
- a_plugin_callback_reaches_only_its_granted_orbit_tools → callback_tool_authorization. Named table row; original body and assertions retained.
- a_plugin_child_cannot_forge_its_orbit_tools_allowlist → callback_tool_authorization. Named table row; original body and assertions retained.
- a_plugin_callback_cannot_exceed_the_spawning_callers_tool_ceiling → callback_tool_authorization. Named table row; original body and assertions retained.
- a_global_mcp_plugin_backend_receives_its_activity_callback_ceiling → callback_identity_confinement. Named table row; original body and assertions retained.
- a_plugin_child_cannot_clear_orbit_plugin_to_escape_its_allowlist → callback_identity_confinement. Named table row; original body and assertions retained.
- an_mcp_backend_plugin_is_advertised_and_proxied → callback_identity_confinement. Named table row; original body and assertions retained.
- an_mcp_backend_receives_its_declared_secret_in_meta_and_no_response_holds_it → plugin_backend_secrets. Named table row; original body and assertions retained.
- an_mcp_backend_rotation_is_stored_and_no_response_or_audit_row_holds_a_value → plugin_backend_secrets. Named table row; original body and assertions retained.
- a_plugin_child_cannot_shed_its_callback_session_with_setsid → mcp_backend_discovery_and_secret_redaction. Named table row; original body and assertions retained.
- a_plugin_holding_orbit_tools_cannot_write_orbits_own_roots → mcp_backend_discovery_and_secret_redaction. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/mcp/mcp_roundtrip/desktop.rs](../../crates/orbit-cli/tests/mcp/mcp_roundtrip/desktop.rs)

- desktop_writes_reconcile_after_restart_and_reject_stale_or_implicit_destinations → desktop_destination_and_routine_contracts. Named table row; original body and assertions retained.
- domain_automation_stdio_preserves_observed_routine_state_and_refuses_dispatch_without_authority → desktop_destination_and_routine_contracts. Named table row; original body and assertions retained.
- replica_routine_control_enables_only_worktree_gc_and_names_the_owner_for_the_rest → desktop_destination_and_routine_contracts. Named table row; original body and assertions retained.
- desktop_governed_status_approval_and_crew_preserve_receipts_and_review_gates → desktop_governance_and_bounded_reads. Named table row; original body and assertions retained.
- bounded_run_detail_reads_a_fixed_audit_budget_beyond_a_long_history → desktop_governance_and_bounded_reads. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/mcp/mcp_roundtrip/transport_operations.rs](../../crates/orbit-cli/tests/mcp/mcp_roundtrip/transport_operations.rs)

- stdio_artifact_put_reopens_intact_bytes_and_refuses_workspace_escape → artifact_and_auto_task_stdio_contracts. Named table row; original body and assertions retained.
- stdio_auto_task_crud_mints_without_dispatch_and_reopens_definition_state → artifact_and_auto_task_stdio_contracts. Named table row; original body and assertions retained.
- stdio_drain_and_ship_starts_report_their_notices → artifact_and_auto_task_stdio_contracts. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/task/task_trimmed_surface.rs](../../crates/orbit-cli/tests/task/task_trimmed_surface.rs)

- update_status_reopens_terminal_tasks_only_with_an_explicit_force → governed_status_transitions. Named table row; original body and assertions retained.
- update_status_restores_an_archived_task_with_force → governed_status_transitions. Named table row; original body and assertions retained.
- update_status_refuses_completion_without_review_and_evidence → governed_status_transitions. Named table row; original body and assertions retained.
- update_status_performs_approve_and_reject_transitions → governed_status_transitions. Named table row; original body and assertions retained.
- update_discard_candidate_records_the_discard_unless_a_run_is_active → candidate_and_context_updates. Named table row; original body and assertions retained.
- task_update_repeats_dependencies_and_clears_context → candidate_and_context_updates. Named table row; original body and assertions retained.
- task_update_rejects_missing_context_file_and_leaves_task_unchanged → candidate_and_context_updates. Named table row; original body and assertions retained.
- task_update_and_add_accept_valid_context_selectors → candidate_and_context_updates. Named table row; original body and assertions retained.
- tool_task_update_context_preserves_omissions_replaces_lists_and_clears → scope_readiness_and_creation_grants. Named table row; original body and assertions retained.
- task_show_and_write_tools_report_readiness_gaps → scope_readiness_and_creation_grants. Named table row; original body and assertions retained.
- backlog_writes_without_context_warn_about_the_missing_file_lock → scope_readiness_and_creation_grants. Named table row; original body and assertions retained.
- allow_missing_context_records_durable_exact_creation_intent → scope_readiness_and_creation_grants. Named table row; original body and assertions retained.
- locks_list_projects_files_held_by_active_tasks → lock_and_audit_administration. Named table row; original body and assertions retained.
- locks_release_reaches_the_admin_tool_only_with_the_operator_capability → lock_and_audit_administration. Named table row; original body and assertions retained.
- audit_prune_refuses_unconfirmed_then_deletes_when_confirmed → lock_and_audit_administration. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/task/pilot_comments.rs](../../crates/orbit-cli/tests/task/pilot_comments.rs)

- approval_prints_latest_pilot_blocker_warnings_in_text_and_json → pilot_comment_rendering. Named table row; original body and assertions retained.
- task_show_summarizes_pilot_comments_without_changing_json_or_fallback → pilot_comment_rendering. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/task/auto_task_lifecycle_cli.rs](../../crates/orbit-cli/tests/task/auto_task_lifecycle_cli.rs)

- auto_task_cli_delete_opt_out_restore_and_open_task_refusals_persist → auto_task_lifecycle_and_recovery. Named table row; original body and assertions retained.
- auto_task_cli_recovery_and_reset_preview_preserve_then_audit_consumer_changes → auto_task_lifecycle_and_recovery. Named table row; original body and assertions retained.
- auto_task_cli_delete_failing_after_consumer_reset_keeps_definition_and_cursor → auto_task_lifecycle_and_recovery. Named table row; original body and assertions retained.
- auto_task_waive_batch_reports_an_unregistered_required_tool_as_a_warning → auto_task_lifecycle_and_recovery. Named table row; original body and assertions retained.
- auto_task_add_and_schedule_update_reject_retired_integrated_qa_coverage → retired_coverage_and_closed_actions. Named table row; original body and assertions retained.
- auto_task_cli_reset_recover_and_doctor_treat_a_closed_action_as_settled → retired_coverage_and_closed_actions. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/task/context_selector_worktree.rs](../../crates/orbit-cli/tests/task/context_selector_worktree.rs)

- selector_missing_from_both_checkouts_is_rejected_from_a_worktree → worktree_scope_and_owned_creation. Named table row; original body and assertions retained.
- worktree_nested_under_the_registered_checkout_can_declare_its_own_file → worktree_scope_and_owned_creation. Named table row; original body and assertions retained.
- owning_worker_in_its_worktree_may_declare_a_file_it_has_not_created_yet → worktree_scope_and_owned_creation. Named table row; original body and assertions retained.
- owning_worker_cli_update_gets_the_same_relaxation_as_the_tool → worktree_scope_and_owned_creation. Named table row; original body and assertions retained.
- owning_worker_cannot_relax_a_task_its_run_does_not_own → foreign_and_invalid_creation_refusals. Named table row; original body and assertions retained.
- owning_worker_relaxation_still_rejects_invalid_selectors → foreign_and_invalid_creation_refusals. Named table row; original body and assertions retained.
- owning_worker_outside_a_worktree_gets_the_strict_guard → foreign_and_invalid_creation_refusals. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/task/friction_lifecycle_cli.rs](../../crates/orbit-cli/tests/task/friction_lifecycle_cli.rs)

- friction_update_empty_title_restores_derivation_like_the_tool → friction_clear_semantics_and_replica_admission. Named table row; original body and assertions retained.
- friction_update_empty_rehome_to_clears_alone_and_with_other_edits → friction_clear_semantics_and_replica_admission. Named table row; original body and assertions retained.
- friction_optional_blank_strings_without_clear_semantics_remain_omitted → friction_clear_semantics_and_replica_admission. Named table row; original body and assertions retained.
- replica_closes_legacy_local_frictions_with_audit_without_owner_mutations → friction_clear_semantics_and_replica_admission. Named table row; original body and assertions retained.
- friction_cli_triage_stats_and_rehome_preserve_workspace_ownership → friction_ownership_and_redaction. Named table row; original body and assertions retained.
- friction_title_is_redacted_on_add_and_update_before_persistence → friction_ownership_and_redaction. Named table row; original body and assertions retained.
- friction_add_description_alias_is_redacted_like_body → friction_ownership_and_redaction. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/task/write_preflight.rs](../../crates/orbit-cli/tests/task/write_preflight.rs)

- add_warnings_match_on_cli_and_tool → write_preflight_cli_tool_parity. Named table row; original body and assertions retained.
- update_and_approval_warnings_match_on_cli_and_tool → write_preflight_cli_tool_parity. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/task/task_list.rs](../../crates/orbit-cli/tests/task/task_list.rs)

- completed_add_and_update_leave_the_next_write_free_list_indexed → task_index_and_bounded_listing. Named table row; original body and assertions retained.
- task_list_truncation_notice_and_bare_json_array_with_60_tasks → task_index_and_bounded_listing. Named table row; original body and assertions retained.
- task_list_truncation_notice_reaches_stderr_in_json_and_ndjson_modes → task_index_and_bounded_listing. Named table row; original body and assertions retained.
- older_someday_task_discoverable_ahead_of_newer_done_tasks → task_index_and_bounded_listing. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/task/auto_task_settings_cli.rs](../../crates/orbit-cli/tests/task/auto_task_settings_cli.rs)

- template_flags_preserve_unnamed_fields_and_full_tool_templates_still_replace → auto_task_settings_and_managed_bodies. Named table row; original body and assertions retained.
- bundled_auto_task_settings_stay_outside_the_body_and_survive_a_body_refresh → auto_task_settings_and_managed_bodies. Named table row; original body and assertions retained.
- settings_only_fork_migrates_and_a_body_fork_is_preserved_and_reported → auto_task_settings_and_managed_bodies. Named table row; original body and assertions retained.
- older_shipped_bodies_upgrade_with_settings_and_custom_bodies_stay_forked → auto_task_settings_and_managed_bodies. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/task/task_tags.rs](../../crates/orbit-cli/tests/task/task_tags.rs)

- task_cli_roundtrips_filters_and_replaces_tags → task_tags_status_filters_and_identity. Named table row; original body and assertions retained.
- search_status_modes_include_blocked_and_deferred_tasks → task_tags_status_filters_and_identity. Named table row; original body and assertions retained.
- task_list_is_status_neutral_and_bounded_by_limit → task_tags_status_filters_and_identity. Named table row; original body and assertions retained.
- task_update_refuses_dropping_system_identity_tag_without_override → task_tags_status_filters_and_identity. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/task/local_read_projections_cli.rs](../../crates/orbit-cli/tests/task/local_read_projections_cli.rs)

- local_read_projections_return_registered_keys_task_flow_and_artifact_manifest → local_read_projection_contracts. Named table row; original body and assertions retained.
- task_show_formats_status_history_and_whole_second_timestamps_without_changing_json → local_read_projection_contracts. Named table row; original body and assertions retained.
- task_show_and_list_json_match_the_tool_host_projection → local_read_projection_contracts. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/task/task_admin_cli.rs](../../crates/orbit-cli/tests/task/task_admin_cli.rs)

- task_updates_and_approval_redact_prose_before_persistence → task_admin_persistence_and_conflicts. Named table row; original body and assertions retained.
- reservation_cli_conflicts_and_confirmation_preserve_claims → task_admin_persistence_and_conflicts. Named table row; original body and assertions retained.
- portable_archive_cli_round_trip_and_corrupt_input_preserve_tasks → task_admin_persistence_and_conflicts. Named table row; original body and assertions retained.
- lock_contention_cli_reports_shared_backlog_surface_without_reserving → task_admin_persistence_and_conflicts. Named table row; original body and assertions retained.

### [crates/orbit-cli/tests/task/claimed_delivery_recovery.rs](../../crates/orbit-cli/tests/task/claimed_delivery_recovery.rs)

- terminal_claim_adopts_settings_and_doctor_reports_its_status → claimed_recovery_contracts_0. Named table row, with unchanged assertions inside the same isolated-child boundary.
- archived_claim_reissues_the_same_obligations_as_a_new_task → claimed_recovery_contracts_0. Named table row, with unchanged assertions inside the same isolated-child boundary.
- unminted_retry_recovers_after_a_legacy_task_closed_and_settings_changed → claimed_recovery_contracts_0. Named table row, with unchanged assertions inside the same isolated-child boundary.
- scheduler_settles_closed_claims_and_adopts_but_refuses_open_tasks → claimed_recovery_contracts_1. Named table row, with unchanged assertions inside the same isolated-child boundary.
- version_one_receipts_still_read_as_accepted_coverage → claimed_recovery_contracts_0. The existing archived-claim rows run all accepted-version-one assertions using their already-accepted receipt, preserving the post-retry read without another fixture.

### [crates/orbit-cli/tests/task/delivery_remote_source.rs](../../crates/orbit-cli/tests/task/delivery_remote_source.rs)

- remote_only_landing_mints_a_batch_without_moving_the_checkout → remote_delivery_contracts_0. Named table row, with unchanged assertions inside the same isolated-child boundary.
- fetch_failure_defers_without_falling_back_to_the_local_branch → remote_delivery_contracts_0. Named table row, with unchanged assertions inside the same isolated-child boundary.
- force_pushed_remote_history_defers_as_history_diverged → remote_delivery_contracts_0. Named table row, with unchanged assertions inside the same isolated-child boundary.
- stamp_coverage_is_refused_and_per_delivery_evidence_settles → remote_delivery_contracts_1. Named table row, with unchanged assertions inside the same isolated-child boundary.
- fetch_failure_during_evidence_verification_defers_without_spending_a_retry → remote_delivery_contracts_1. Named table row, with unchanged assertions inside the same isolated-child boundary.
- doctor_reports_wedged_coverage_and_inspects_adoption_without_fetching → remote_delivery_contracts_1. Named table row, with unchanged assertions inside the same isolated-child boundary.
- diverged_history_rejects_stopped_evidence_as_source_unverifiable → remote_delivery_contracts_2. Named table row, with unchanged assertions inside the same isolated-child boundary.
- doctor_reports_the_remote_tracking_head_without_fetching → remote_delivery_contracts_2. Named table row, with unchanged assertions inside the same isolated-child boundary.
- doctor_uses_the_oldest_pending_commit_for_remote_trail_health → remote_delivery_contracts_2. Named table row, with unchanged assertions inside the same isolated-child boundary.

### [crates/orbit-cli/tests/task/delivery_remote_source/review_commit.rs](../../crates/orbit-cli/tests/task/delivery_remote_source/review_commit.rs)

- review_coverage_bound_to_the_batch_is_the_no_diff_evidence → review_commit_contracts. Named table row, with unchanged assertions inside the same isolated-child boundary.
- a_derived_summary_leaves_the_batch_owed → review_commit_contracts. Named table row, with unchanged assertions inside the same isolated-child boundary.
- an_agent_replacement_of_a_derived_summary_settles_the_batch → review_commit_contracts. Named table row, with unchanged assertions inside the same isolated-child boundary.

### [crates/orbit-cli/tests/task/delivery_remote_source/tick.rs](../../crates/orbit-cli/tests/task/delivery_remote_source/tick.rs)

- clock_tick_expired_source_preserves_debt_without_running_commands → clock_source_contracts. Named table row, with unchanged assertions inside the same isolated-child boundary.
- clock_tick_shares_fetch_and_each_lookup_across_consumers → clock_source_contracts. Named table row, with unchanged assertions inside the same isolated-child boundary.
- clock_tick_leaves_two_parked_consumers_with_24_obligations_untouched → clock_source_contracts. Named table row, with unchanged assertions inside the same isolated-child boundary.

### [crates/orbit-cli/tests/task/config_show_task_reads.rs](../../crates/orbit-cli/tests/task/config_show_task_reads.rs)

- one_config_show_takes_the_task_commit_lock_once → review_after_landing_cli::after_landing_review_mints_exactly_when_the_auto_task_is_enabled. All lock-count and projection assertions run against an equivalent fresh runtime over the already-minted review fixture; no duplicate workspace, commit or review-task setup.

