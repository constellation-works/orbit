# Exemplars — STD-03 Concurrency and process safety

Worked examples of the rules in [STD-03](STD-03.md), as pointers into the Orbit
codebase. Orbit is the example, not the audience: this file is optional reading, and
useful only with an Orbit checkout.

Paths are relative to `codebases/orbit/`.

- R1 — `Cargo.toml#[workspace.lints.clippy]` (`await_holding_lock = "deny"`);
  `crates/orbit-web/src/runtime_memo.rs#RuntimeMemo` (std `Mutex` for the map, a
  `tokio::sync::Mutex` per-key single-flight gate for the compute that awaits);
  `crates/orbit-common/src/test_env.rs#ScopedEnv` (its doc comment says to drop the guard before
  an await); `crates/orbit-exec/src/supervision/signal.rs#SignalHandlerGuard` (install mutex
  held only for the refcount section, never across a child's lifetime);
  `crates/orbit-web/src/api/tasks.rs` (`tokio::task::spawn_blocking` around runtime calls).
- R2 — `crates/orbit-web/src/api/log.rs#spawn_log_sse_frames` (`LOG_STREAM_CHANNEL_DEPTH`,
  `blocking_send`) and `#stream_log` (refuses when no `LogStreamGate` permit is free);
  `crates/orbit-tools/src/plugin/mcp.rs` (`sync_channel(LINE_QUEUE)`).
- R3 — `docs/design-patterns/task_commit_boundary.md#The two mechanisms`;
  `crates/orbit-store/src/repository/task/coordination/boundary.rs#TaskCommitBoundary::enter_ordinary`
  and `#with_admission`.
- R4 — `docs/design-patterns/raii_guard.md`; `crates/orbit-cli/src/audit_middleware.rs#AuditGuard`;
  `crates/orbit-common/src/fs/io.rs#StagedTextFile`.
- R5 — `crates/orbit-common/src/fs/io.rs#atomic_write_bytes` (durable) and
  `#atomic_write_text_volatile`; `crates/orbit-store/src/fs/yaml.rs#write_yaml_durable_with`;
  `crates/orbit-store/src/driver/file/task_bundle/bundle_io/write.rs#write_bundle_atomically`
  (a multi-file bundle staged in a sibling directory, synced, then renamed).
- R6 — `crates/orbit-common/src/fs/file_lock.rs#try_acquire_exclusive_file_lock` (the `O_CLOEXEC`
  and inherited-descriptor notes, ORB-12532); `crates/orbit-store/src/fs/lock/mod.rs#FileLockGuard`;
  `crates/orbit-registry/src/workspace_registry/io.rs#with_registry_lock` (wraps the whole
  load-edit-save); `crates/orbit-store/src/driver/sqlite/migration/ledger.rs#apply_one`
  (`BEGIN IMMEDIATE`, then re-reads the version inside the transaction).
- R7 — `crates/orbit-common/src/fs/file_lock.rs#DEFAULT_FILE_LOCK_TIMEOUT` and `#FileLockTimeout`
  (carries `holder`), and `#FileLockHolderInfo` ("advisory metadata").
- R8 — `crates/orbit-store/src/repository/task/coordination/commit.rs#commit_task_transition`;
  `crates/orbit-store/src/driver/sqlite/task_commit_journal/mod.rs`; for a single store,
  `crates/orbit-store/src/driver/file/task_bundle/bundle_io/commit.rs#PendingWriteGuard`.
- R9 — `crates/orbit-store/src/repository/task/coordination/commit.rs#recover` and
  `#recover_if_pending`; `docs/design-patterns/task_commit_boundary.md#The two mechanisms`
  ("Recovery before exposure").
- R10 — `crates/orbit-store/src/contracts/compat.rs#evaluate_newer_state`;
  `crates/orbit-store/src/driver/sqlite/connection.rs` (`query_only` pinning);
  `docs/design/state-compatibility/2_design.md#3 The decision` and `#4 Read-only is enforced, not promised`;
  `docs/design/state-compatibility/4_decisions.md` ("Declare `Breaking` when in doubt").
- R11 — `crates/orbit-exec/src/process.rs#command` (`command.process_group(0)`).
- R12 — `crates/orbit-exec/src/supervision/cleanup.rs#terminate_process_group`
  (`TERMINATION_GRACE_PERIOD`) and `#process_group_is_alive` (anything but `ESRCH` is alive);
  `docs/design/policy-sandbox/1_overview.md#2.5 Exec supervision is not default OS isolation`;
  `docs/design/policy-sandbox/2_design.md#8. Process Supervision`.
- R13 — `crates/orbit-exec/src/supervision/wait.rs#wait_with_timeout_and_output_limit`
  (clean-exit `kill_process_group` before the reader joins) and `#WaitResult` (`timed_out`
  beside `exit_code`).
- R14 — `crates/orbit-exec/src/supervision/signal.rs#SignalHandlerGuard` (`release_process_group`);
  `crates/orbit-exec/src/supervision/tests/signal.rs`;
  `crates/orbit-common/src/process/identity.rs#probe_process_liveness` and `#ProbeOutcome`
  (PID plus a versioned start-identity token with its PID namespace; an unavailable probe is not
  proof of death).
- R15 — `crates/orbit-exec/src/supervision/tee.rs#spawn_stdout_drain`;
  `docs/design/policy-sandbox/specs/sandbox-exec-contract.md#Supervision Invariants`;
  `crates/orbit-engine/src/activity_job/job_executor/recovery.rs#bounded_recovery_text`
  (`MAX_RECOVERY_ERROR_MESSAGE_BYTES`, marked truncation, full text in the run audit).
- R16 — `crates/orbit-core/src/application/job/pipeline/worker/scope.rs#contain_worker_command`
  and `#WorkerLimits`; `crates/orbit-core/src/application/job/pipeline/worker/supervisor.rs#spawn_process`
  (`setsid` plus `register_worker_process`).
- R17 — `crates/orbit-cli/tests/web_serve_root.rs#wait_for_listening` (deadline plus
  `try_wait`); `crates/orbit-common/src/test_process.rs#retry_executable_busy`.
- R18 — `crates/orbit-core/src/application/job/run/tests/owner.rs#ReapingChild`;
  `crates/orbit-cli/tests/mcp_roundtrip.rs#McpClient` (`Drop` kills and waits).
- R19 — `crates/orbit-core/src/application/job/pipeline/worker/command.rs#refuse_test_harness_worker`
  and `#worker_command_override`; `crates/orbit-core/src/lib.rs` (`test_support::install_substitute_pipeline_worker`);
  `docs/DEVELOPMENT.md#Tests that submit pipeline runs`.
- R20 — `docs/DEVELOPMENT.md#Safe Mutable CLI Fixtures`;
  `crates/orbit-common/src/test_env.rs#clear_inherited_authority`;
  `crates/orbit-cli/tests/ambient_authority_isolation.rs`; `.config/nextest.toml` (`stateful-runtime`
  test group).
- R21 — `docs/LESSONS.md#3. The September 2026 Test Fixture Fork Storm`;
  `crates/orbit-core/src/application/job/pipeline/worker/scope.rs` (module doc).
- R22 — `crates/orbit-config/src/registry/settings.rs#DEFAULT_WORKER_TASKS_MAX` (with
  `DEFAULT_WORKER_MEMORY_HIGH` and `DEFAULT_WORKER_MEMORY_MAX`);
  `crates/orbit-exec/src/supervision/wait.rs#wait_with_optional_timeout`;
  `crates/orbit-tools/src/plugin/mcp.rs` (module doc: "no code path that waits without a
  deadline"; a reader thread makes the per-request `recv_timeout` real);
  `crates/orbit-mcp/src/federated/probe.rs#DEFAULT_ROUTED_DELIVERY_TIMEOUT` (budget measured
  from when the request is written, not from session start).
- R23 — `crates/orbit-store/src/driver/sqlite/migration/ledger.rs#MIGRATIONS`,
  `#validate_registry` (strictly increasing) and `#apply_one` (one transaction with its ledger
  row); `crates/orbit-store/src/driver/sqlite/migration/tests/ledger.rs`
  (`shipped_v1_baseline_structure_is_frozen`, `v1_upgrade_and_fresh_database_have_identical_columns`).
- R24 — `crates/orbit-common/src/process/identity.rs#STABLE_TOKEN_PREFIX_V1` (old format read,
  never written); `crates/orbit-config/src/crew_pools.rs#reject_unpoolable_crew_name_in_config`
  (the fallback: the refusal names the file and the table to edit by hand).
- R25 — `crates/orbit-core/src/application/managed_assets.rs#reconcile_managed_assets_in_mode`
  and `#preserve_modified_retired_asset` (`.orbit-managed-assets.json` digests);
  `crates/orbit-core/src/application/routines/tests/materialize.rs`.
- R26 — `crates/orbit-cmd/src/update/mod.rs` (module doc: the replacement executable migrates
  state and reconciles managed assets); `crates/orbit-cmd/src/update/converge.rs`;
  `crates/orbit-common/src/fs/generation.rs#GenerationGuard`; `docs/runbooks/upgrades.md`.
- R27 — `crates/orbit-engine/src/executor/automation/vcs/worktree/setup.rs#setup_worktree`
  (resolves `base_sha` once, ORB-10380);
  `crates/orbit-engine/src/executor/automation/vcs/commit/mod.rs#validate_pinned_head` and
  `#pinned_object_id` (rejects a ref name where the pinned id belongs).
- R28 — `crates/orbit-engine/src/activity_job/workspace/boundary_guard.rs#verify_after_provider`,
  `#primary_stationary_dirt_delta_is_benign` and `#primary_fast_forward_is_benign`;
  `crates/orbit-engine/src/activity_job/cli_runner/tests/orchestrator_worktree.rs`
  (`two_in_flight_worktrees_survive_one_primary_merge_advance`).
- R29 — `crates/orbit-store/src/driver/file/task_bundle/bundle_io/stub.rs#is_unpublished_stub`
  and `#reap_unpublished_stub` (unreadable or data-bearing directories are kept);
  `crates/orbit-cmd/src/doctor/task.rs#doctor_check_orphan_task_stores`;
  `docs/design/activity-job/4_decisions.md` ("On timeout, mutate only state this attempt owns").
- R30 — `crates/orbit-engine/src/executor/automation/vcs/failure.rs#pr_failure_handoff`
  (`commit_failure_candidate` before push and PR);
  `crates/orbit-engine/src/executor/automation/vcs/pr/tests/handoff.rs`
  (`non_fast_forward_drift_handoff_commits_dirty_work_and_raises_pr`).
- R31 — `crates/orbit-mcp/src/federated/probe.rs#call_tool` (`LostAnswer::OutcomeUnknown`) and
  `#DEFAULT_ROUTED_DELIVERY_TIMEOUT`; `crates/orbit-mcp/src/federated/host.rs#delivery_unreachable`
  (never relabels a dispatched call); `docs/design/distributed-drain/4_decisions.md`.
- R32 — `crates/orbit-store/src/workflow/task/publish.rs#observe_remote` (fetches before
  judging the tip), `#push_fast_forward` and `#compare_and_swap_lease`;
  `crates/orbit-engine/src/executor/automation/vcs/operations.rs#push`
  (`--force-with-lease=refs/heads/<branch>:<expected_remote_sha>`).
