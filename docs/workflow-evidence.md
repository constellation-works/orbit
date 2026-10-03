# Shipped workflow evidence

The shipped catalog is `DEFAULT_JOB_FILES` in
[`runtime/assets.rs`](../crates/orbit-core/src/runtime/assets.rs): 15 jobs;
`assets/jobs/examples/` is excluded. A catalog parse or a persisted row bearing
one of these names does **not** prove that its shipped graph executed.

The table maps the strongest identified behavioral evidence, not a blanket
runtime pass. “Graph” means the resolved shipped graph executes under the job
engine, with the fixture substitutions described below. “Boundary” means an
admission, action, dispatch or recovery contract executes, without proving the
whole shipped graph. Run the named test to obtain current execution evidence.

Core paths below are relative to `crates/orbit-core/src/`; Engine paths are
relative to `crates/orbit-engine/src/`.

| Shipped job | Evidence level and named behavior | Owning test source |
| --- | --- | --- |
| `agent_invoke_pipeline` | Graph through MCP stdio: `a_remote_operator_session_invokes_an_agent_end_to_end` submits an invocation, polls terminal success and verifies the completed envelope plus persisted trusted-host admission. Provider executables return planted responses; the shipped graph is not replaced. Refusal: `a_remote_agent_session_cannot_invoke_an_agent`. Core admission refusals: `a_runs_own_runner_grant_cannot_admit_an_invocation`, `an_admitted_run_cannot_be_resumed`, `an_unauthorized_keyed_submission_claims_nothing`. | `crates/orbit-cli/tests/mcp_roundtrip.rs`; Core `application/job/tests/agent_invoke.rs` |
| `ci_failure_sweep_pipeline` | Boundary: `no_current_failure_is_a_clean_no_op_and_not_a_capability_problem`; `a_second_sweep_over_a_still_red_run_does_not_file_a_second_task`; `failed_or_warned_pilot_does_not_block_an_independent_eligible_fix`. Filing and admission actions, not the shipped graph. | Core `adapter/engine_host/v2_host/ci_failure/tests/{filing,admission}.rs` |
| `dependabot_alert_sweep_pipeline` | Boundary: `http_403_and_404_are_distinct_capability_outcomes_not_no_alerts`; `several_alerts_for_one_dependency_file_exactly_one_evidence_complete_task`. Collector and filing are tested separately. | Engine `executor/automation/dependabot/tests.rs`; Core `adapter/engine_host/v2_host/dependabot/tests/filing.rs` |
| `task_auto_pipeline` | Boundary: `the_child_run_is_durable_and_linked_before_the_wait_begins`; `a_failed_child_stays_linked_with_its_terminal_status`; `mixed_crew_child_failure_closes_dispatch_and_fails_parent_guard_promptly`; `pipeline_success_guard_rejects_mixed_results`. Dispatch and success-guard actions, not the shipped graph. | Core `adapter/engine_host/v2_host/pipeline_actions/tests/{invoke,results}.rs` |
| `task_claimed_local_pipeline` | No graph or refusal evidence. `claimed_leaves_and_pending_admissions_consume_the_legacy_drain_ceiling` only counts claimed leaves against the drain ceiling. | Core `adapter/engine_host/v2_host/workspace_auto/tests/classify.rs` |
| `task_claimed_pr_pipeline` | Boundary: `pull_pr_mode_binds_the_claimed_pr_leaf_with_no_completion_authority`. `distributed_drain.rs` claims this job as its leaf, but the leaf launch is refused there, so the graph never runs. | Core `adapter/engine_host/v2_host/pull/tests/drain.rs`; `crates/orbit-core/tests/distributed_drain.rs` |
| `task_gate_pipeline` | Boundary: `an_eligible_bundle_still_dispatches_after_the_gate_waited`; `already_shipped_work_still_reports_a_succeeded_noop`; `a_withdrawn_dispatch_result_releases_the_reservation_then_fails_the_gate`; `reserve_locks_records_unmet_dependencies_in_run_state`. | Core `adapter/engine_host/v2_host/pipeline_actions/tests/gate_admission.rs`, `adapter/engine_host/v2_host/tests/dispatch.rs` |
| `task_landing_pipeline` | Boundary, store level: `one_open_attempt_per_handoff_survives_restart_and_reopens_only_deliberately`; `revoked_authority_stops_dispatch_and_completion_for_the_same_candidate`; `replaced_handoff_evidence_blocks_approval_and_landing`. These prove landing admission, not dispatch or execution of the landing graph. | `crates/orbit-store/src/repository/task/coordination/tests/landing.rs`; `crates/orbit-store/tests/allocation_admission.rs` |
| `task_local_pipeline` | Boundary: `actual_ship_pipeline_implementers_run_for_each_provider_and_stay_in_the_worktree` loads the shipped definition but runs only its implement step through the CLI backend. | Engine `activity_job/cli_runner/tests/orchestrator_worktree.rs` |
| `task_pilot_pipeline` | Boundary: `shipped_task_pilot_binds_omitted_optional_base_branch_and_preserves_override` resolves the shipped templating; `replay_returns_already_applied_without_a_second_mutation`; `material_criteria_edit_invalidates_real_pilot_apply`. | Engine `activity_job/job_executor/tests/templating.rs`; Core `adapter/engine_host/v2_host/task_pilot/tests/{apply,source}.rs` |
| `task_pr_pipeline` | Boundary: `conflict_recovery_prepares_assigned_context_and_persists_launch_failure` and `step_failure_recovery_keeps_managed_context_for_implement_and_commit` resolve the shipped definition for recovery preparation; `managed_completion_pins_the_reviewed_head_and_records_the_landing`; `already_landed_retry_completes_without_new_commit_or_pr_and_is_idempotent`. External actions are scripted. | Core `adapter/engine_host/v2_host/tests/v2_host.rs`; Engine `executor/automation/vcs/{pr/tests/review_gate,commit/tests/already_landed}.rs` |
| `workspace_auto_pipeline` | Boundary: `a_dispatch_that_never_produced_a_child_fails_instead_of_waiting`; `a_detached_child_is_recorded_as_non_blocking`; `invoke_detached_skips_when_the_parent_has_stopped_admissions`; `a_raised_ceiling_is_observed_by_the_next_admission_pass`. | Core `adapter/engine_host/v2_host/pipeline_actions/tests/invoke.rs`, `adapter/engine_host/v2_host/workspace_auto/tests/classify.rs` |
| `workspace_pull_pipeline` | Boundary: `pull_lost_request_and_binding_responses_recover_the_same_leaf`; `pull_transport_failure_keeps_the_request_pending_for_the_same_id`; `pull_stop_preserves_children_and_local_binding_refuses_generic_execution`; across two composed runtimes, `lost_pull_and_bind_replies_recover_the_same_claim_and_leaf_exactly_once` and `a_settlement_whose_reply_is_lost_is_redelivered_and_applied_once`. Executes pull/refill/settlement actions, not the shipped polling graph. | Core `adapter/engine_host/v2_host/pull/tests/drain.rs`; `crates/orbit-core/tests/distributed_drain.rs` |
| `workspace_ship_pipeline` | Boundary: `workspace_ship_input_prefers_the_registry_neutral_runtime_binding`; `workspace_ship_input_uses_the_registered_base_branch_over_workflow_config`. Input resolution only. | Core `adapter/engine_host/v2_host/tests/dispatch.rs` |
| `worktree_gc_pipeline` | Graph through CLI: `routine_dispatch_ignores_ambient_orbit_root_and_reaps_only_the_owning_workspaces_worktree` runs a registered routine and verifies terminal success plus actual local worktree removal. Boundary refusal: `worktree_gc_attempts_no_deletion_for_rejected_values`. | `crates/orbit-cli/tests/worktree_gc_routing.rs`; Engine `executor/automation/tests/worktree_gc.rs` |

## Shared recovery evidence

These supplement the rows; they are not execution proof for every shipped job:

- `crates/orbit-cli/tests/mcp_roundtrip/transport_operations.rs`:
  `stdio_resume_runs_only_deterministic_remaining_steps_and_reopens_checkpoint_lineage`
  resumes a fixture job over MCP stdio from its own checkpoints.
- Engine `activity_job/job_executor/tests/resume.rs`:
  `resume_skips_checkpointed_steps_and_feeds_their_outputs`,
  `resume_starts_at_the_failed_push_and_reuses_checkpoints_idempotently`.
- Core `adapter/engine_host/v2_host/pull/tests/drain.rs`:
  `pull_stop_preserves_children_and_local_binding_refuses_generic_execution`
  refuses generic resume and execution of a claimed leaf.
- Engine `activity_job/job_executor/tests/recovery.rs`:
  `declared_failed_outcome_after_retry_exhaustion_recovers_once_and_keeps_diagnostic`.
- Engine `executor/automation/vcs/pr/tests/resume_refresh.rs`:
  `recovered_candidate_survives_restart_and_failure_handoff_before_step_checkpoint`.
  These execute recovery on synthetic jobs and local repositories.

## Safe focused execution

These suites use temporary stores, scripted hosts/providers and local Git
repositories. They do not authorize a live workflow or provider dispatch.
Record the actual test counts and results separately; a filter matching zero
tests is not evidence. Coordinate Cargo target use before running them.

```sh
cargo test -p orbit-cli --test mcp_roundtrip
cargo test -p orbit-cli --test worktree_gc_routing
cargo test -p orbit-core --test distributed_drain
cargo test -p orbit-core --lib application::job::tests::agent_invoke
cargo test -p orbit-core --lib adapter::engine_host::v2_host
cargo test -p orbit-engine --lib activity_job::job_executor::tests
cargo test -p orbit-engine --lib executor::automation
cargo test -p orbit-store --lib repository::task::coordination::tests::landing
```

Only `agent_invoke_pipeline` and `worktree_gc_pipeline` have shipped-graph
execution evidence. Full shipped-graph fixtures remain a coverage opportunity
for every other job; report the action and admission tests above at their
actual boundary.
