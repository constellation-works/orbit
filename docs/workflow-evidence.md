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
| `ci_failure_sweep_pipeline` | Boundary: `ci_failure_fixture_goldens` drives `file_ci_failure_tasks` through `OrbitRuntime::run_deterministic` over captured log shapes, reads the filed tasks back and reruns each scenario to verify persisted deduplication. Filing only; pilot admission and the shipped graph have no current test. | `crates/orbit-core/tests/ci_failure_goldens.rs` |
| `dependabot_alert_sweep_pipeline` | Boundary: `sentinel_credential_never_reaches_snapshot_output_or_persisted_task_fields` files from a collected snapshot and checks that a credential never reaches output or task fields. Per-file grouping, unavailable families and the collector have no current test. | Core `adapter/engine_host/v2_host/dependabot/tests/filing.rs` |
| `task_auto_pipeline` | Boundary: `backlog_admission_orders_critical_then_corrective_then_priority_then_age`, `backlog_admission_waits_for_every_dependency_to_be_done` and `backlog_admission_excludes_work_locked_by_an_active_task` exercise the backlog selection every drain runs. Child dispatch, linking and the success guard have no current test. | `crates/orbit-core/tests/dispatch_admission.rs` |
| `task_claimed_local_pipeline` | No graph, boundary or refusal evidence. | — |
| `task_claimed_pr_pipeline` | Boundary: `distributed_drain.rs` claims this job as its leaf, but the leaf launch is refused there, so the graph never runs. Claimed-PR binding has no current test. | `crates/orbit-core/tests/distributed_drain.rs` |
| `task_gate_pipeline` | Boundary: `backlog_admission_waits_for_every_dependency_to_be_done`; `backlog_admission_excludes_work_locked_by_an_active_task`; `a_held_workspace_claim_gates_dispatch_to_its_holder`. Reservation release and no-op reporting have no current test. | `crates/orbit-core/tests/dispatch_admission.rs` |
| `task_landing_pipeline` | Boundary, store level: `one_open_attempt_per_handoff_survives_restart_and_reopens_only_deliberately`; `revoked_authority_stops_dispatch_and_completion_for_the_same_candidate`; `replaced_handoff_evidence_blocks_approval_and_landing`. These prove landing admission, not dispatch or execution of the landing graph. | `crates/orbit-store/src/repository/task/coordination/tests/landing.rs`; `crates/orbit-store/tests/allocation_admission.rs` |
| `task_local_pipeline` | Boundary: `a_shell_step_gets_only_the_policy_baseline_and_its_explicit_env` exercises the explicit environment exposed to a local shell step; it does not execute the shipped graph. | `crates/orbit-engine/tests/v2_local_shell.rs` |
| `task_pilot_pipeline` | No graph or boundary evidence; apply replay and source invalidation have no current test. | — |
| `task_pr_pipeline` | No graph or boundary evidence; recovery context preparation for the shipped definition has no current test. | — |
| `workspace_auto_pipeline` | Boundary: `dispatch_admission.rs` backlog admission and `a_held_workspace_claim_gates_dispatch_to_its_holder` / `an_expired_workspace_claim_stops_gating_dispatch`. Detached child dispatch and the admission ceiling have no current test. | `crates/orbit-core/tests/dispatch_admission.rs` |
| `workspace_pull_pipeline` | Boundary: `pull_lost_request_and_binding_responses_recover_the_same_leaf`; across two composed runtimes, `lost_pull_and_bind_replies_recover_the_same_claim_and_leaf_exactly_once` and `a_settlement_whose_reply_is_lost_is_redelivered_and_applied_once`. Executes pull/refill/settlement actions, not the shipped polling graph. | Core `adapter/engine_host/v2_host/pull/tests/drain.rs`; `crates/orbit-core/tests/distributed_drain.rs` |
| `workspace_ship_pipeline` | No graph or boundary evidence; input resolution has no current test. | — |
| `worktree_gc_pipeline` | Graph through CLI: `routine_dispatch_ignores_ambient_orbit_root_and_reaps_only_the_owning_workspaces_worktree` runs a registered routine and verifies terminal success plus actual local worktree removal. Boundary retention: `worktree_gc_keeps_every_protected_checkout_and_reaps_settled_ones`. | `crates/orbit-cli/tests/worktree_gc_routing.rs`; `crates/orbit-engine/tests/v2_worktree_lifecycle.rs` |

## Shared recovery evidence

These supplement the rows; they are not execution proof for every shipped job:

- `crates/orbit-cli/tests/mcp_roundtrip/transport_operations.rs`:
  `stdio_resume_runs_only_deterministic_remaining_steps_and_reopens_checkpoint_lineage`
  resumes a fixture job over MCP stdio from its own checkpoints.
- Engine `activity_job/job_executor/tests/resume.rs`:
  `resume_reexecuted_pr_output_reaches_promotion_and_checkpoint`.
- `crates/orbit-engine/tests/v2_worktree_lifecycle.rs`:
  `conflict_recovery_leaf_completes_only_its_checkpointed_rebase`.
  These execute recovery on synthetic jobs and local repositories.

## Safe focused execution

These suites use temporary stores, scripted hosts/providers and local Git
repositories. They do not authorize a live workflow or provider dispatch.
Record the actual test counts and results separately; a filter matching zero
tests is not evidence. Coordinate Cargo target use before running them.

```sh
cargo test -p orbit-cli --test mcp_roundtrip
cargo test -p orbit-cli --test worktree_gc_routing
cargo test -p orbit-core --test ci_failure_goldens
cargo test -p orbit-core --test dispatch_admission
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
