# Machine checks — STD-03 Concurrency and process safety

The gate that enforces each rule in [STD-03](STD-03.md), or `review-only`. Ship a change
with the gate its rule names, or say it is review-only.

Each gate below is real and can be adopted. "Orbit:" says whether Orbit enforces it today.

| Rule | Gate |
|---|---|
| R1 | Rust: set `clippy::await_holding_lock = "deny"` and `clippy::await_holding_refcell_ref` in `[workspace.lints.clippy]`, and run clippy with `-D warnings`. For other guard types, use `clippy::await_holding_invalid_type` with `await-holding-invalid-types` in `clippy.toml`. None of these lints sees `tokio::sync` guards, which are allowed by design. Orbit: `await_holding_lock` is denied; the other two are not set. Locks and transactions across slow work, and blocking calls on async threads: review-only in every language. |
| R2 | Rust: `clippy::disallowed_methods`, listing `std::sync::mpsc::channel`, `tokio::sync::mpsc::unbounded_channel` and `crossbeam_channel::unbounded` under `disallowed-methods` in `clippy.toml`. The full-queue behaviour is review-only. Orbit: not configured, so review-only. |
| R3 | review-only |
| R4 | Rust: the rustc lint `let_underscore_lock` (deny by default) catches a guard dropped immediately. Put `#[must_use]` on guard types so `unused_must_use` fires. Everything else: review-only. |
| R5 | Rust: `clippy::disallowed_methods` listing `std::fs::write` (and any other direct-write entry point the project wants funnelled) in `clippy.toml`, with `#[allow(clippy::disallowed_methods)]` only inside the helper module. Orbit: not configured, so review-only. |
| R6 | review-only; add a test that runs two read-modify-write updaters concurrently and asserts neither update is lost |
| R7 | review-only; add a unit test that covers the timeout path and asserts the holder appears in the error |
| R8 | review-only; add fault-injection tests that crash before and after the commit point (Orbit: `crates/orbit-store/src/repository/task/coordination/faults.rs`) |
| R9 | Same fault-injection tests as R8, asserting that a failed replay leaves the store refused, not open |
| R10 | Test: stamp a newer version and a compatibility record into a fixture store, then assert a read-only open with writes refused, or a refusal (Orbit: `crates/orbit-store/src/driver/sqlite/migration/tests/ledger.rs`, `crates/orbit-store/src/workflow/layout/tests/mod.rs`). The compatibility classification itself is review-only. |
| R11 | review-only; add a test that a grandchild dies with its group |
| R12 | review-only; add a test that a SIGTERM-ignoring child is SIGKILLed after the grace period |
| R13 | Rust: `clippy::zombie_processes` flags a spawned child that is never waited on (Orbit: not set explicitly). The group sweep and the termination report are review-only; add a test that a child killed on timeout is not reported as success. |
| R14 | Test: signal only live, owned group leaders (Orbit: `crates/orbit-exec/src/supervision/tests/signal.rs`); a live PID with a foreign start-identity token reads as exited (Orbit: `crates/orbit-common/src/process/tests/identity.rs`, `live_pid_with_a_foreign_identity_token_reads_as_exited`) |
| R15 | review-only; add a test with a child that writes more than the pipe buffer, and one that feeds an oversized error through every path that embeds it (Orbit: `crates/orbit-engine/src/activity_job/job_executor/tests/recovery.rs`) |
| R16 | Runtime: launch into a scope with limits, for example `systemd-run --user --scope -p TasksMax=… -p MemoryMax=…`. Orbit: on by default through `machine.worker_containment`. It falls back to the caller's cgroup with a warning when no user manager is reachable. |
| R17 | review-only. An optional CI grep that rejects `sleep 0.0` inside shell `while` loops in test sources would catch the known pattern. Orbit: no such guard. |
| R18 | Rust: `clippy::zombie_processes` is a partial check. Otherwise review-only. |
| R19 | Runtime: fail closed in the spawn path when the executable is a test harness, plus a test that asserts the refusal (Orbit: `refuse_test_harness_worker`) |
| R20 | nextest `[test-groups]` with `max-threads = 1` for tests that change global state (Orbit: `stateful-runtime`). Isolating `HOME` and authority variables is review-only. |
| R21 | Test runner: nextest `slow-timeout = { period = "60s", terminate-after = 2 }` terminates a hung test, and `leak-timeout = { period = "500ms", result = "fail" }` fails a test whose children keep its output open. Runtime: tear down the run's own cgroup or unit when the run ends. Orbit: a per-worker scope; nextest timeouts are not configured. |
| R22 | systemd `TasksMax=`, `MemoryMax=` and `RuntimeMaxSec=` on units and scopes; GitHub Actions `timeout-minutes`; nextest `slow-timeout` and `global-timeout`; pytest `--timeout` from pytest-timeout. Orbit: worker-scope limits are on; CI jobs have no `timeout-minutes`, and nextest has no `slow-timeout`. In-process deadlines: review-only; add a test with a peer that stalls after accepting a request (Orbit: `crates/orbit-mcp/src/federated/tests/probe.rs`). |
| R23 | Test: a frozen structural fingerprint of the released baseline, plus a test that a fresh store and one upgraded from the oldest supported version have identical schemas (Orbit: `shipped_v1_baseline_structure_is_frozen`, `v1_upgrade_and_fresh_database_have_identical_columns`). Runtime: refuse a migration registry whose versions are not strictly increasing (Orbit: `validate_registry`). |
| R24 | review-only; add a fixture of state written by the previous release and assert that the new binary loads it, or refuses naming the file (Orbit: `crates/orbit-config/src/tests/layering.rs`, `a_persisted_colon_named_crew_is_refused_by_file_and_repaired_by_renaming_it`) |
| R25 | Test: an unmodified retired default is retired, and a user-modified one is preserved (Orbit: `crates/orbit-core/src/application/routines/tests/materialize.rs`). Otherwise review-only. |
| R26 | review-only; the deploy procedure runs the new binary's migrate and reconcile steps and restarts long-lived units (Orbit: `orbit update` convergence steps) |
| R27 | Runtime: steps that need the pinned value accept only an immutable id and reject a ref name (Orbit: `pinned_object_id`), plus a test that a sibling moving the named ref does not fail an in-flight run |
| R28 | Test: an unrelated concurrent change to shared state does not trip the guard, and an overlapping one does (Orbit: `two_in_flight_worktrees_survive_one_primary_merge_advance`, `concurrent_primary_fast_forward_does_not_block_disjoint_worktree_changes`) |
| R29 | review-only; add tests that an unreadable, partial or data-bearing target survives the cleanup path (Orbit: `crates/orbit-store/src/repository/task/tests/v2_bundle.rs`, `listing_reports_data_bearing_bundle_missing_envelope`) |
| R30 | review-only; add a fault-injection test that fails a step after work exists and asserts the work is recoverable after teardown (Orbit: `non_fast_forward_drift_handoff_commits_dirty_work_and_raises_pr`) |
| R31 | Test: a peer that stalls or disconnects after the request is written yields outcome unknown, not unreachable (Orbit: `crates/orbit-mcp/src/federated/tests/route.rs`, `a_dispatched_call_whose_answer_is_lost_is_outcome_unknown_not_unreachable`; `crates/orbit-mcp/src/federated/tests/probe.rs`, `a_stall_after_the_tool_call_is_dispatched_is_outcome_unknown`). Automatic reclaim: review-only. |
| R32 | Git: `push --force-with-lease=<ref>:<expected-sha>` (never a bare `--force`) and `--atomic` for several refs; `gh pr merge --match-head-commit <sha>` for a pinned merge. Test: a remote tip that is deleted, rewound or advanced after observation is a conflict (Orbit: `crates/orbit-store/src/workflow/task/tests/publish.rs`). Fetch before judging containment: review-only. |
