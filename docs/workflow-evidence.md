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
| `agent_invoke_pipeline` | Graph through MCP stdio: `a_remote_operator_session_invokes_an_agent_end_to_end` submits an invocation, polls terminal success and verifies the completed envelope plus persisted trusted-host admission. Provider executables return planted responses; the shipped graph is not replaced. Refusal: `a_remote_agent_session_cannot_invoke_an_agent`. Admission/idempotency remain covered separately by Core tests. | `crates/orbit-cli/tests/mcp_roundtrip.rs`; Core `application/job/tests/agent_invoke.rs` |
| `ci_failure_sweep_pipeline` | Graph: `ci_sweep_parent_fails_for_stale_pilot_after_independent_pilot_applies`; `ci_sweep_parent_keeps_a_genuinely_empty_pilot_batch_as_a_successful_no_op`. Collector is a recording fixture. | Core `application/job/tests/exec/ci_sweep.rs` |
| `dependabot_alert_sweep_pipeline` | Boundary: `http_403_and_404_are_distinct_capability_outcomes_not_no_alerts`; `several_alerts_for_one_dependency_file_exactly_one_evidence_complete_task`. Collector and filing are tested separately. `dependabot_sweep_pipeline_is_two_deterministic_steps_and_single_flight` parses the graph. | Engine `executor/automation/dependabot/tests.rs`; Core `adapter/engine_host/v2_host/dependabot/tests/filing.rs`, `application/job/tests/catalog.rs` |
| `task_auto_pipeline` | Graph: `shipped_task_auto_dispatches_each_bundle_with_the_operator_contract`; `shipped_task_auto_fails_after_collecting_a_failed_gate_with_other_results`; `shipped_task_auto_empty_backlog_succeeds_without_a_child`; `shipped_task_auto_refuses_unknown_bundle_before_dispatching_a_child`. Backlog and child results are scripted; bundle validation and parent success guard are real. | Core `application/job/tests/exec/task_auto.rs` |
| `task_claimed_local_pipeline` | Graph: `owner_local_claim_executes_validates_and_hands_off_without_merging`; refusal: `a_claimed_attempt_reporting_failure_is_refused_before_any_git_mutation`; recovery: `fault_injection_at_every_cut_yields_at_most_one_leaf_per_claim`. | Core `application/job/tests/exec/claimed_leaf.rs` |
| `task_claimed_pr_pipeline` | Graph: `a_published_pr_claim_hands_off_a_pull_request_without_merging`; retry: `a_published_pr_retry_publishes_past_the_previous_attempts_failed_summary`. Local bare Git remote and fixture `gh`; no GitHub request. | Core `application/job/tests/exec/claimed_leaf.rs` |
| `task_gate_pipeline` | Graph: `task_gate_dispatches_child_for_admissible_task`; `task_gate_noops_done_task_and_releases_reservation`; `task_gate_child_failure_still_fails_success_guard`. Scripted child results. | Core `application/job/tests/exec.rs` |
| `task_landing_pipeline` | Boundary: `an_approved_handoff_dispatches_exactly_one_owner_landing_job`; `a_pending_request_whose_job_never_started_is_recovered_by_the_next_pass`. Worker is a shell fixture; these prove dispatch/outbox recovery, not execution of the landing graph. | Core `application/landing/tests/dispatch.rs` |
| `task_local_pipeline` | Graph: `gated_local_ship_without_a_remote_never_pushes_or_recovers`; `gated_local_ship_still_pushes_when_the_caller_asks_for_it`. Gate and local child resolve shipped definitions; scripted implementation and local Git. | Core `application/job/tests/exec/local_ship.rs` |
| `task_pilot_pipeline` | Graph: `shipped_pipeline_repairs_only_invalid_task_and_preserves_partial_progress`; `real_cli_task_pilot_worker_persists_apply_and_terminal_completion_audit`. The latter invokes a local fake provider executable, not a remote model. | Core `application/job/tests/task_pilot_pipeline.rs` |
| `task_pr_pipeline` | Graph: `before_pr_changes_required_blocks_the_task_without_opening_a_pr`; `completion_merge_failure_routes_to_review_recovery_without_republishing`; `already_landed_pipeline_routes_verified_evidence_without_a_new_delivery`. External actions are scripted. | Core `application/job/tests/exec/{review_gate,completion,already_landed}.rs` |
| `workspace_auto_pipeline` | Graph: `workspace_auto_keeps_dispatching_while_earlier_leaves_are_still_running`; `workspace_auto_fails_promptly_when_leaf_dispatch_has_no_durable_child`; `workspace_auto_preserves_concrete_workspace_step_failure`. Detached child admission is scripted. | Core `application/job/tests/workspace_auto_pipeline.rs` |
| `workspace_pull_pipeline` | Boundary: `pull_lost_request_and_binding_responses_recover_the_same_leaf`; `pull_transport_failure_keeps_the_request_pending_for_the_same_id`; `pull_stop_preserves_children_and_local_binding_refuses_generic_execution`. Executes pull/refill/settlement actions, not the shipped polling graph. | Core `adapter/engine_host/v2_host/pull/tests/drain.rs` |
| `workspace_ship_pipeline` | Graph: `shipped_workspace_ship_waits_for_child_before_guarding_its_receipt`; `shipped_workspace_ship_propagates_child_failure`; `shipped_workspace_ship_stops_before_guard_when_child_dispatch_fails`. Child dispatch is synthetic; receipt interpolation, sequencing and success guard are real. | Core `application/job/tests/exec/workspace_ship.rs` |
| `worktree_gc_pipeline` | Graph through CLI: `routine_dispatch_ignores_ambient_orbit_root_and_reaps_only_the_owning_workspaces_worktree` runs a registered routine and verifies terminal success plus actual local worktree removal. Boundary refusal: `worktree_gc_attempts_no_deletion_for_rejected_values`. | `crates/orbit-cli/tests/worktree_gc_routing.rs`; Engine `executor/automation/tests/worktree_gc.rs` |

## Shared recovery evidence

These supplement the rows; they are not execution proof for every shipped job:

- Core `application/job/tests/resume.rs`:
  `pipeline_worker_resumes_from_the_runs_own_checkpoints`,
  `claimed_leaf_refuses_generic_resume_but_same_run_evidence_retries_work`,
  `direct_yaml_resume_uses_the_pinned_definition_after_the_source_file_is_removed`.
  The direct-YAML test intentionally uses a fixture graph.
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
cargo test -p orbit-core --lib application::job::tests::exec
cargo test -p orbit-core --lib application::job::tests::workspace_auto_pipeline
cargo test -p orbit-core --lib application::job::tests::task_pilot_pipeline
cargo test -p orbit-core --lib application::job::tests::agent_invoke
cargo test -p orbit-core --lib application::job::tests::resume
cargo test -p orbit-core --lib application::tests::job_pipeline
cargo test -p orbit-core --lib application::landing::tests
cargo test -p orbit-cli --test worktree_gc_routing
```

Remaining coverage opportunities are full shipped-graph fixtures for
Dependabot sweep, task landing and workspace pull. Existing action/admission
tests remain valuable, but should be reported at their actual boundary.
