---
type: context
summary: "Evidence map for shipped Orbit jobs and the coverage boundaries of their named tests."
last_updated: 2026-10-12
last_validated: 2026-10-12
---

# Shipped workflow evidence

The process-boundary fixtures in `crates/orbit-cli/tests/process/` run update-root routing, stale-updater refusal, live-run cancellation and dashboard handover on Linux and macOS. `mcp_roundtrip::upgrade` also verifies drain handover and safe-point yielding across both hosts. Handover reads a live process's locked image registration rather than `/proc/<pid>/exe`; the updater race uses a nonblocking FIFO rendezvous available on both systems. The macOS workflow runs these suites. Linux `/proc` owner/namespace proofs and the Linux-only privileged host-preparation opt-out retain explicit gates, and Darwin libproc probes retain theirs.

The shipped catalog is `DEFAULT_JOB_FILES` in
[`runtime/assets.rs`](../../crates/orbit-core/src/runtime/assets.rs): 21 jobs;
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
| `agent_invoke_pipeline` | Graph through MCP stdio: `a_remote_operator_session_invokes_an_agent_end_to_end` submits an invocation, polls terminal success and verifies the completed envelope plus persisted trusted-host admission. Provider executables return planted responses; the shipped graph is not replaced. Refusal: `a_remote_agent_session_cannot_invoke_an_agent`. Core admission refusals: `a_runs_own_runner_grant_cannot_admit_an_invocation`, `an_admitted_run_cannot_be_resumed`, `an_unauthorized_keyed_submission_claims_nothing`. | `crates/orbit-cli/tests/mcp/mcp_roundtrip.rs`; Core `application/job/tests/agent_invoke.rs` |
| `baseline_hold_refresh_pipeline` | Graph through the owner sweep runtime: `a_hold_check_runs_off_the_tick_and_ticks_meanwhile_fire_routines_and_auto_tasks` dispatches and executes the shipped refresh job outside the tick, records its verdict and lets other clock work continue. `a_checked_tip_is_judged_from_its_recorded_result_without_another_run` and `an_inconclusive_tip_waits_for_backoff_or_a_new_tip_before_another_refresh` cover recorded results and bounded retry. | `crates/orbit-core/tests/runtime/baseline_hold_tick.rs` |
| `blocked_task_recovery_pipeline` | Graph through the owner runtime: `recovery_uses_machine_bound_run_evidence_and_its_own_crew_draw` runs the shipped recovery job for a settled follower failure, checks machine-bound failure evidence and crew selection, and verifies requeue. | `crates/orbit-core/tests/runtime/distributed_drain/recovery.rs` |
| `ci_failure_sweep_pipeline` | Boundary: `ci_failure_fixture_goldens` drives `file_ci_failure_tasks` through `OrbitRuntime::run_deterministic` over captured log shapes, including rustfmt diffs ending in punctuation, symbols-only excerpts falling back to the step, and unusable signatures retaining exact-key deduplication without aborting unrelated filings. It reads the filed tasks back and reruns scenarios to verify persisted deduplication; `ci_failure_predating_a_landed_repair_is_held_not_refiled` holds a failure tested before a done repair of the same signature landed and files one tested after it. `ci_failure_claimed_branch_defers_attribution_until_the_claim_settles` keeps a follower-claimed task-branch failure out of the owner's bundle, files the landing failure in the same snapshot, and writes the observation when the claim is released. Collection's in-flight-descendant hold is covered by `orbit-engine`'s `executor::automation::ci::tests::pending`. Filing only; pilot admission and the shipped graph have no current test. | `crates/orbit-core/tests/ci_failure_goldens.rs` |
| `dependabot_alert_sweep_pipeline` | Boundary: `sentinel_credential_never_reaches_snapshot_output_or_persisted_task_fields` files from a collected snapshot and checks that a credential never reaches output or task fields. Per-file grouping, unavailable families and the collector have no current test. | `crates/orbit-core/tests/runtime/admission_audit/dependabot.rs` |
| `finding_verification_pipeline` | Graph through the job engine: `shipped_job_applies_successful_partitions_and_retains_a_failed_partitions_error` applies the successful partition while retaining the failed partition's error; `shipped_job_applies_escalation_verdicts_and_empty_selection_is_a_noop` verifies escalation, recording and empty-selection behavior. Verifier responses are fixture-controlled. | `crates/orbit-core/tests/runtime/finding_verification.rs` |
| `review_evidence_fulfilment_pipeline` | Graph through the runtime: `a_held_codeql_check_runs_at_the_held_commit_and_its_result_requeues_review` runs the shipped fulfilment job with a stub CodeQL executable at the held commit and verifies accepted evidence and requeue. `an_incomplete_failed_or_unadmitted_run_leaves_the_hold_with_a_typed_reason` covers refusal outcomes; real CodeQL is not run. | `crates/orbit-core/tests/runtime/review_evidence_fulfilment.rs` |
| `store_gc_pipeline` | Graph through CLI: `audit_retention_plans_then_prunes_rows_and_unreferenced_blobs` runs the shipped job with `orbit job run --wait` after seeding the store, and verifies terminal success with both applied reports in its pipeline state. The same test proves the `gc audit` apply it wraps. | `crates/orbit-cli/tests/workspace/store_gc.rs` |
| `task_auto_pipeline` | Boundary: `backlog_admission_orders_critical_then_corrective_then_priority_then_age`, `backlog_admission_waits_for_every_dependency_to_be_done` and `backlog_admission_excludes_work_locked_by_an_active_task` exercise the backlog selection every drain runs. Child dispatch, linking and the success guard have no current test. | `crates/orbit-core/tests/runtime/dispatch_admission.rs` |
| `task_claimed_local_pipeline` | Resumed graph: `verified_no_diff_and_already_satisfied_base_complete_without_a_pr` executes the shipped delivery steps after a real commit verifier and checks owner settlement without a PR. `a_landing_stopped_on_its_base_is_repaired_and_lands_under_the_same_task` resumes after fixture implementation, executes the shipped commit, validation and handoff steps, and verifies repair on a moved base. No real implementer provider runs. | `crates/orbit-core/tests/runtime/distributed_drain/no_diff.rs`; `crates/orbit-core/tests/runtime/distributed_drain/landing_repair.rs` |
| `task_claimed_pr_pipeline` | Resumed graph: `verified_no_diff_and_already_satisfied_base_complete_without_a_pr` covers the clean delivery branch. `a_forge_outage_holds_the_claim_and_the_same_head_reaches_pr_open` resumes the shipped push/PR steps from fixture validation checkpoints, uses local Git and a stub `gh`, and verifies that the same head reaches the remote and PR output. It truncates after `pr_open` and shortens retry waits; no real reviewer or GitHub runs. | `crates/orbit-core/tests/runtime/distributed_drain/no_diff.rs`; `crates/orbit-core/tests/runtime/distributed_drain/forge_hold.rs` |
| `task_gate_pipeline` | Boundary: `backlog_admission_waits_for_every_dependency_to_be_done`; `backlog_admission_excludes_work_locked_by_an_active_task`; `a_held_workspace_claim_gates_dispatch_to_its_holder`. Reservation release and no-op reporting have no current test. | `crates/orbit-core/tests/runtime/dispatch_admission.rs` |
| `task_landing_pipeline` | Boundary: `a_landing_stopped_on_its_base_is_repaired_and_lands_under_the_same_task` finds the dispatched landing attempt and executes its `handoff_land` action directly, proving a stopped local fast-forward, repair and eventual landing. `a_second_base_stop_blocks_the_task_with_both_attempts_evidence` verifies the repair bound. Store admission: `handoff_authorization_follows_the_owner_policy_at_admission` and `replaced_handoff_evidence_blocks_approval_and_landing`. These do not execute the standalone landing graph through the job engine. | `crates/orbit-core/tests/runtime/distributed_drain/landing_repair.rs`; `crates/orbit-store/tests/store/allocation_admission.rs` |
| `task_local_pipeline` | Boundary: `a_shell_step_gets_only_the_policy_baseline_and_its_explicit_env` exercises the explicit environment exposed to a local shell step; it does not execute the shipped graph. | `crates/orbit-engine/tests/engine/v2_local_shell.rs` |
| `task_pilot_pipeline` | Graph, empty selection: `all_held_pilot_run_succeeds_and_retains_every_holder_skip` runs the shipped job, retains every exclusion and verifies unchanged tasks without invoking a pilot agent. Boundary: `mixed_pilot_selection_applies_free_tasks_and_skips_held_tasks` applies fixture pilot results; `context_and_instruction_edits_skip_stale_partitions_and_requeue_without_failures` verifies source invalidation. Real pilot-provider execution is not established by these proofs. | `crates/orbit-core/tests/runtime/task_pilot.rs`; `crates/orbit-core/tests/runtime/task_pilot/source_moves.rs` |
| `task_pr_pipeline` | No graph or boundary evidence; recovery context preparation for the shipped definition has no current test. | — |
| `task_review_reconciliation_pipeline` | Graph in-process: `distributed_drain::desktop_completion` submits it through `orbit.task.reconcile_review` and runs every step with the in-process worker. The run executes real required commands at the merged head and base, a stub reviewer provider on the configured crew, and settlement. The tests cover accepted, rejected-with-follow-up, baseline-disposition and changed-head outcomes, and an unadmitted run that cannot touch the record. The detached worker process and a real reviewer model are untested. | `crates/orbit-core/tests/runtime/distributed_drain/desktop_completion.rs` |
| `workspace_auto_pipeline` | Graph through the runtime: `backlog_work_is_admitted_while_the_approval_pilot_runs_and_the_drain_waits_for_it` executes the shipped job, proves admission and refill while a pilot is running, and waits for that pilot after admissions close. Child workers are substituted and pilot output is fixture data. Boundary: `dispatch_admission.rs` backlog admission and `a_held_workspace_claim_gates_dispatch_to_its_holder` / `an_expired_workspace_claim_stops_gating_dispatch`. | `crates/orbit-core/tests/runtime/drain_approval/step_graph.rs`; `crates/orbit-core/tests/runtime/dispatch_admission.rs` |
| `workspace_pull_pipeline` | Graph through the runtime: `a_windowless_pull_drain_claims_up_to_its_slots_once_and_then_only_settles` executes the shipped polling job in-process with substituted child workers, proving bounded admission and settlement. Boundary: `pull_lost_request_and_binding_responses_recover_the_same_leaf`; across two composed runtimes, `lost_pull_and_bind_replies_recover_the_same_claim_and_leaf_exactly_once` and `a_settlement_whose_reply_is_lost_is_redelivered_and_applied_once`. No real SSH or provider runs. | `crates/orbit-core/tests/runtime/distributed_drain/single_pass.rs`; Core `application/pull_drain/tests/drain.rs`; `crates/orbit-core/tests/runtime/distributed_drain/` |
| `workspace_ship_pipeline` | No graph or boundary evidence; input resolution has no current test. | — |
| `worktree_gc_pipeline` | Graph through CLI: `routine_dispatch_ignores_ambient_orbit_root_and_reaps_only_the_owning_workspaces_worktree` runs a registered routine and verifies terminal success plus actual local worktree removal. Boundary retention: `worktree_gc_keeps_every_protected_checkout_and_reaps_settled_ones`. Scratch pruning through the shipped job: `prunes_entries_older_than_the_window_and_never_follows_symlinks` (24-hour default, override, newest-mtime boundary, symlinks), `an_entry_a_live_process_has_open_or_uses_as_its_cwd_is_skipped_and_reported` (Linux), `a_negative_window_is_refused_before_anything_is_removed`. | `crates/orbit-cli/tests/workspace/worktree_gc_routing.rs`; `crates/orbit-cli/tests/workspace/scratch_gc.rs`; `crates/orbit-engine/tests/engine/v2_worktree_lifecycle.rs` |

## Shared recovery evidence

These supplement the rows; they are not execution proof for every shipped job:

- `crates/orbit-cli/tests/mcp/mcp_roundtrip/transport_operations.rs`:
  `stdio_resume_runs_only_deterministic_remaining_steps_and_reopens_checkpoint_lineage`
  resumes a fixture job over MCP stdio from its own checkpoints.
- Engine `activity_job/job_executor/tests/resume.rs`:
  `resume_reexecuted_pr_output_reaches_promotion_and_checkpoint`.
- `crates/orbit-engine/tests/engine/v2_worktree_lifecycle.rs`:
  `conflict_recovery_leaf_completes_only_its_checkpointed_rebase`.
  These execute recovery on synthetic jobs and local repositories.

## Safe focused execution

These suites use temporary stores, scripted hosts/providers and local Git
repositories. They do not authorize a live workflow or provider dispatch.
Record the actual test counts and results separately; a filter matching zero
tests is not evidence. Coordinate Cargo target use before running them.

```sh
cargo test -p orbit-cli --test mcp mcp_roundtrip::
cargo test -p orbit-cli --test workspace worktree_gc_routing::
cargo test -p orbit-cli --test workspace store_gc::
cargo test -p orbit-core --test ci_failure_goldens
cargo test -p orbit-core --test runtime dispatch_admission::
cargo test -p orbit-core --test runtime distributed_drain::
cargo test -p orbit-core --lib application::job::tests::agent_invoke
cargo test -p orbit-engine --lib activity_job::job_executor::tests
cargo test -p orbit-engine --test engine v2_worktree_lifecycle::
cargo test -p orbit-store --test store allocation_admission::
```

Graph evidence applies only to the paths and fixture substitutions each row
describes. Resumed graphs do not prove the checkpointed steps executed in that
run, and an empty-selection graph does not prove agent execution. Full graph
and provider fixtures remain coverage opportunities; report action and
admission tests at the boundary they exercise.
