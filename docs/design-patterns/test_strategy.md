---
type: pattern
summary: "Boundary-First Testing: integration, golden and e2e by default; unit tests by exception"
last_validated: 2026-10-12
---
# Boundary-First Testing

Test behaviour where callers meet it. New and changed behaviour is proven at a stable boundary by default: a crate-root integration test, a golden, or an e2e flow. A unit test is the exception, and it must earn its place by meeting one of the admission criteria below.

## Why

On 2026-10-03, unit tests under `crates/*/src/**` were about 295k lines and 7,675 test functions, more lines than the production code they covered. Crate-root integration tests were 37k lines. Most unit tests pinned internals, so a refactor that kept behaviour unchanged still broke dozens of them, and each break cost an agent tokens to repair a test rather than find a bug. A review of all of them kept 180 (2.3%).

The default is not "no unit tests". The aim is to put each guarantee at the cheapest layer that still proves it:

- A boundary test survives refactors.
- A unit test pins how the code is written today.
- An e2e test is the most expensive and the slowest to localise a failure, so it covers critical flows only.

## The layers

| Layer | Where | Use it for |
|---|---|---|
| **Integration** (default) | `crates/<crate>/tests/` | Behaviour through the crate's public API or built binary: tool dispatch, runtime operations, store transactions, HTTP routes, CLI commands. Hostile state is built directly in the fixture. |
| **Golden** | Beside the integration test that renders it (e.g. `crates/orbit-cli/tests/output_goldens/`); regenerated with `make goldens UPDATE=1` | Output contracts: CLI help and `--json`, MCP tool schemas, effective-config projections, sandbox profile text, parser output over captured real-world input (CI logs, provider streams). |
| **E2E** | `crates/orbit-cli/tests/` against the built `orbit` binary | A small set of critical flows end to end: ship and land, the drain, gates, update. |
| **Unit** (exception) | `<module>/tests/<file>.rs` per [test_layout.md](test_layout.md) | Only what meets the admission criteria below. |

## Unit-test admission criteria

A unit test is allowed only when at least one of these holds and a boundary test cannot prove the same thing economically:

1. **Combinatorial pure logic.** Examples: a state-machine transition table, a parser's edge cases, policy or selector matching, scheduler or backoff arithmetic, cycle detection. Write one table-driven test per invariant, not one test per case.
2. **Deterministic interleaving or fault injection.** The test needs a test seam to force a race, a crash mid-write, a TOCTOU swap or a lost reply.
3. **A security or safety invariant the boundary cannot reach cheaply.** Examples: path or symlink confinement, secret masking across chunk boundaries, privilege propagation, fail-closed validation of a crafted input.
4. **Platform-kernel behaviour.** A real `sandbox-exec` or bwrap result, or process-group reaping, where no integration test runs on that platform.

The test's name or assertion message says which invariant it guards. A test that guards a specific incident cites it.

## Not admitted as unit tests

These are not admitted:

- serde round-trips, defaults, builders, getters and `Display`, unless the test guards a persisted on-disk format that has a migration
- mock-heavy wiring tests that assert which collaborator was called
- argument parsing and output formatting that a golden already covers
- a second test of an invariant that already has one
- behaviour already exercised by an integration test

Regression tests follow STD-04 §R1: drive the entry point production callers use. A bug found at a boundary gets its regression test at that boundary.

## Growth ratchet

`scripts/unit-test-inventory.py` inventories the unit-test functions under each `crates/<crate>/src/**`, inline or in a sibling `tests/` directory. [`scripts/unit-test-baseline.json`](../../scripts/unit-test-baseline.json) records sorted admitted identities as `<path relative to src>::<function name>` per crate (schema version 2). Repeated names in separate inline modules in one file retain their multiplicity. `make ci-fast` fails when a present test is not named in the baseline, even when the crate's total count stays the same. The change that adds or renames unit tests must update the baseline too, so each admission appears as a reviewed diff. Concurrent admissions then either merge with both identities recorded or conflict textually and require reconciliation; identical count bumps can no longer silently lose an admission.

Removing tests still passes with stale baseline entries; those entries do not admit different tests in their place. Run `scripts/unit-test-inventory.py` for the current counts and identities, and `scripts/unit-test-inventory.py --write-baseline scripts/unit-test-baseline.json` to regenerate the baseline, including removing retired identities. The inventory scans test attributes without evaluating `cfg` or expanding macros, as in the original retirement measure.

Admitted unit tests:

- `crates/orbit-common/src/process/supervisor/tests/wait.rs`: `a_stalled_caller_clock_does_not_apply_the_wall_clock_deadline` and `a_jumped_caller_clock_stops_at_the_deadline_without_the_wall_wait` (criterion 2, deterministic clock). An injected clock that stays still must let a child outlive a wall-clock timeout, and one that jumps past the timeout must stop without waiting out that wall time. Automation source measures its budget on this clock; the existing deadline test covers `clock: None`. `redirected_output_stops_when_the_periodic_check_crosses_its_limit` (criterion 4) writes to a caller-owned file, so no capture sink sees the bytes, and asserts the periodic check stops the run with `ProcessStopCause::Periodic` before the deadline.

- `crates/orbit-common/src/process/tests/bounded.rs::bounded_supervision_errors_kill_the_group_and_reap_the_leader` (criterion 2, fault injection). A per-thread seam injects pipe-setup, pipe-read and wait errors through the public `run_bounded` boundary, after placing an independently waitable peer in the child's group. The test verifies the leader is reaped and the peer is killed; descriptor exhaustion or global wait-state corruption would otherwise be needed to reach those errors. The crate-root `process_group_liveness` tests cover owned-handle drop, unwinding, explicit cleanup, TERM escalation and signal guards without injection.

- `crates/orbit-common/src/process/tests/identity.rs::partial_linux_group_scans_cannot_certify_death_and_vanished_pids_do_not_delay_cleanup` (criterion 2, I/O fault injection). The production scan accepts directory entries and a stat reader so the table can force entry errors, unreadable and malformed stat records, and ENOENT between enumeration and reading. It proves an observed zombie cannot certify group death when a potentially live PID cannot be inspected, preserving cleanup's SIGKILL escalation, while vanished PIDs do not prevent zombie-only success. Live-member and unavailable-directory controls cover conclusive activity and the existing signal fallback. The public probe cannot force these partial `/proc` failures economically without host permission changes or nondeterministic races; the crate-root process-group test covers a real Linux zombie through that public probe.

- The [launchd enable tests](../../crates/orbit-core/src/application/routines/clock/tests/enable.rs) (criterion 2,
  fault injection) drives the enable entry point with launchd selected and failed
  program/state probes. It checks rewrite and reload for unhealthy loaded agents,
  error propagation when loading fails, and preservation of healthy and paused
  agents. The public API selects the native host platform, so an integration test
  on Linux cannot force these launchd states.

- `crates/orbit-core/src/application/task/tests/transitions.rs::delete_rechecks_a_status_changed_after_its_guard_and_preserves_force` (criterion 2, deterministic interleaving). The existing transition-read seam commits an in-progress status after deletion's initial proposed/rejected check, before the task write lock. The isolated fixture asserts refusal leaves the task present, with forced deletion and unchanged eligible statuses as controls. Crate-root integration tests cannot reach the `cfg(test)` seam to choose this ordering reliably.

- `crates/orbit-common/src/process/tests/bounded.rs`: `argv_probe_recovers_when_a_live_child_outgrows_the_queried_buffer` (criteria 2 and 4, native fault injection). A pipe handshake grows a live macOS child's argv between the size query and read. The stale buffer yields an unreadable record, which must not prove exit or authorize cleanup; the probe grows its buffer and recovers the recorded identity. Darwin can return success with a truncated `KERN_PROCARGS2` record, so handling only `ENOMEM` is insufficient. The public bounded runner cannot force this observer interleaving.

- `crates/orbit-common/src/process/tests/bounded.rs`: `leak_assertion_retries_unreadable_argv_without_accepting_it_as_reaped` (criterion 2, fault injection). An injected argv probe and clock force a read failure followed by readable live argv, an unreadable probe throughout the grace budget, or a confirmed exit. It checks the leak assertion's panic classification and full retry budget without racing macOS sysctl or generating host load. The public bounded runner cannot force these observer failures; the native blocked-writer test covers pid-only cleanup.

- `crates/orbit-common/src/process/tests/bounded.rs`: `bounded_run_returns_when_a_descendant_holds_either_pipe`, `bounded_run_enforces_deadline_while_a_stream_stays_readable` and `bounded_run_bounds_post_exit_drain_while_a_detached_writer_continues` (criterion 4, platform-kernel behaviour). A descendant keeps one output pipe open after the leader exits; sixteen `cat /dev/zero` writers keep a stream readable past the deadline; `setsid` writers outside the owned group keep filling a pipe during the post-exit drain. Each case's leak check and watchdog are this file's argv-identity harness (`assert_reaped`, `FloodCleanup`). It reads Linux `/proc` or macOS `KERN_PROCARGS2` and signals a pid only when its argv carries the recorded marker, and the admitted cleanup and leak-assertion tests above prove that harness safe. A crate-root copy would duplicate the native harness without those tests, so the public runner is not the cheaper boundary for these three.

- `orbit-engine`'s `branch_lookup_retries_only_supervisor_timeouts_with_a_bounded_budget` (criterion 2, fault injection). A stub Git stalls until the supervisor deadline, then succeeds, exhausts the budget, or completes with a refusal. It drives the branch lookup used by PR preparation and verifies the timeout class, elapsed time, bounded backoff and attempt count. The private Git budget guard scales the deadline; `pr_prepare` does not expose that guard through its public action boundary.

- `crates/orbit-config/src/tests/seed.rs`: `every_family_combination_seeds_valid_defaults_pools_and_enabled_flags` (criterion 1, combinatorial seed rendering) checks all detected-family subsets in memory: defaults name enabled crews, pools parse and reference enabled crews, and each crew's enabled flag follows its provider availability. It avoids filesystem setup for every subset. `rendering_refuses_a_pool_entry_the_seed_does_not_write_enabled` (criterion 2, fault injection) removes or disables a dynamically selected pool member in the private renderer's crew map; the public seeder always constructs that map itself. Neither test fixes crew choices, models or weights as policy.

- `crates/orbit-web/src/log_format/tests/scan.rs`: `post_window_errors_do_not_consume_the_match_budget_or_advance_coverage` (criterion 1, combinatorial scan budgets and coverage). A table drives the existing segmented-reader seam with complete, capped and retention-limited windows and excess newer records. The HTTP boundary fixes the process scan budget at 50,000; this seam proves those cases with a handful of rows, while the HTTP month test covers endpoint wiring and agreement with stderr.

- The four [worker log sibling tests](../../crates/orbit-core/src/application/job/pipeline/worker/tests/log.rs) — `appends_after_the_size_snapshot_are_excluded_from_the_bounded_read`, `empty_short_and_exact_limit_logs_keep_their_expected_content`, `long_logs_report_the_snapshot_window_and_return_only_its_tail`, and `shrink_after_the_size_snapshot_reports_only_bytes_still_available` (criterion 2, deterministic interleaving). A captured size is passed through the same bounded reader used by the audit and supervisor callers; the append case proves bytes written after that snapshot cannot exceed the 16 KiB window, while controls cover empty, short, exact-limit, long and shrinking files. The callers do not lock out the append-oriented worker, and an integration test cannot force the metadata/read order reproducibly [ORB-15610].
- `crates/orbit-core/src/application/job/pipeline/worker/tests/supervisor.rs`: `store_read_faults_retain_supervision_and_reap_in_a_live_parent` (criterion 2, fault injection). A SQLite store wrapper injects unreadable and missing startup records and a post-exit read fault while a socket rendezvous bounds a real child. The isolated parent asserts fresh-state settlement and OS reaping before it exits; the public runtime cannot inject these observer reads.
- The [admissions-stop regressions](../../crates/orbit-core/src/application/job/run/tests/admissions_stop.rs) and `crates/orbit-core/src/application/job/run/tests/drain_cancel.rs`: initialization races (criterion 2, deterministic interleaving). One-shot seams after `NoState` force a worker's first checkpoint and child dispatch, or terminalization with and without that checkpoint, before control initialization. They verify preserved progress and lineage, a merged control for a live drain, and no new effective control for a terminal drain. Crate-root integration tests cannot force this transaction ordering [ORB-15611].
- `crates/orbit-cli/src/tests/main.rs`: `every_assembled_leaf_accepts_json_and_lists_domain_input_exclusions` (criterion 1). It walks the assembled command tree, including hidden and plugin-derived leaves that help goldens cannot enumerate, and asserts that every leaf accepts `--json`. The only exclusion is a plugin tool input named `json`.
- `crates/orbit-cmd/src/doctor/tests/worker_token.rs`: `clock_file_is_ready_only_when_it_holds_a_credential_the_workspace_admits` (criterion 1). It is a table over every pairing of clock-file credential names with the workspace's effective pass, asserting that the clock is ready only when an admitted credential is held, and that a held credential the pass omits is named and warned. The boundary needs an installed launchd or systemd clock to reach this classification. `absent_empty_refused_and_mismatched_clock_files_stay_distinct` (criterion 1) pins that absent, empty, refused and mismatched files stay distinct states.
- `crates/orbit-core/src/runtime/host_resource/tests/platform.rs`: `missing_descendant_reports_its_existing_ancestor_filesystem` and `relative_path_is_refused_instead_of_resolved_against_the_working_directory` (criteria 3 and 4). The disk probe canonicalizes the nearest existing ancestor before statting it, and refuses relative paths so it never resolves against the process working directory. Platform statvfs on a missing worktrees directory has no integration test on any platform.
- `crates/orbit-core/src/runtime/host_resource/tests/platform.rs`: `cpu_window_reuses_advancing_or_repeated_ticks_without_refreshing_the_baseline`, `cpu_window_missing_stale_future_or_identical_baselines_take_a_real_window`, `cpu_window_native_failures_clock_reversal_and_stalls_remain_unknown`, and `cpu_window_handles_tick_wrap_and_the_freshness_boundary` (criteria 1 and 2). Injected counters, timestamps and waits cover XNU cache plateaus, expiry endpoints, failed reads, clock jumps and counter wrap deterministically without generating host load or waiting for a real wrap. The macOS CLI integration test covers actual cross-process history reuse and cold-start recovery.
- `crates/orbit-store/src/driver/sqlite/task_registry/tests/mod.rs`: `fresh_open_through_symlinked_root_matches_reopen_by_canonical_root` (criterion 3). It guards symlink confinement of the registry root: a fresh open through a symlinked root and a later open of the canonical root must key one `workspaces_dir`, because the repair gate keys its attempt budget on that path. `workspaces_dir` is crate-private, so no public-surface test can observe the split.
- `crates/orbit-store/src/repository/task/coordination/tests/admission.rs`: `a_grant_never_takes_a_footprint_the_owner_wave_admitted_before_its_section` and `an_owner_wave_after_a_grant_refuses_the_claimed_footprint` (criterion 2, deterministic interleaving). The first admits an owner drain wave through the crate-private `after_selection` seam, between a pull grant's selection and its exclusive section, and asserts the grant neither takes the admitted task nor one overlapping its footprint. The second commits the grant first and asserts the wave's section refuses the claimed footprint even though the wave's snapshot predates the claim. At the runtime boundary the claim's task is already `in-progress` before any later drain snapshot, and nothing can land a grant between a wave's snapshot and its section, so neither ordering is reachable there.
- `crates/orbit-store/src/repository/task/coordination/tests/claim_section.rs`: `a_claim_mutation_section_decodes_its_own_claim_and_syncs_only_its_commit` and `a_reader_waits_out_a_slow_claim_commit_and_no_longer` (criterion 2, fault injection). The first counts, through the crate-private section probe, the claim-lifecycle rows decoded and the durability syncs requested inside a bind's and a budgeted release's exclusive section among other bound claims, asserts both stay under the pre-change baseline, and asserts an exact retry takes no exclusive section. The second holds a claim commit's apply on a release channel through a `#[cfg(test)]` seam. It waits for the concurrent `get_task`'s partition-lock contention diagnostic before releasing the writer, proves the read has not returned while the bind is half-applied, and checks the committed bind after release. A 200 ms contention-warning threshold gives the apply step diagnostic time, and the hold report must attribute the slowest step to apply; reader assertions use channel ordering and load-tolerant hang guards, with no sleep or cross-thread completion-time bounds [ORB-15820]. Neither the counters nor the stall seam exist in the build the crate-root tests compile against [ORB-15797].
- `crates/orbit-engine/src/executor/automation/vcs/tests/base_claim.rs`: `a_candidate_rerun_stopped_at_the_reserve_still_attributes_its_failures_to_the_base` (criterion 2, fault injection). It stops a candidate rerun that hangs after its failures are printed, using a replay budget of seconds, and asserts that settlement still replays only the printed failing tests on the base and names the cut. The boundary sets the budget in whole review minutes, so reaching the cut there would cost a quarter of a minute or more of wall time per run.
- `crates/orbit-engine/src/executor/automation/vcs/tests/base_claim.rs`: `supervisor_stops_never_classify_partial_failures_as_known_red` (criterion 2, fault injection). In an isolated child, a lowered capture ceiling forces an actual output-capture stop with only 2 KiB of output; a self-signalled run and a completed run are controls. It checks that the shared known-red predicate rejects supervisor stops and base-claim classification refuses them before replay. The settlement boundary covers the signal, but cannot reach the shared predicate after the early refusal [ORB-15590].
- `crates/orbit-common/src/fs/generation/tests/handoff.rs`: `a_probe_that_times_out_is_retried_at_the_next_boundary` and `a_completed_refusal_is_not_probed_again` (criterion 2, fault injection). A candidate's first exec can be held past the probe bound by the OS, which the boundary cannot reproduce on demand, and the process-wide refusal cache is keyed by the replaced image of the running test binary. The first holds a probe past a short timeout against a script that stalls, asserts the timeout is not cached, then answers under a longer budget and asserts that answering call probes exactly once; the second asserts that wrong-contract, missing-capability and non-zero-exit answers are probed once. Both answering probes get the longer budget, so neither passing assertion waits on a short window.
- `crates/orbit-core/src/adapter/engine_host/v2_host/task_pilot/tests/persist.rs`: `overlapping_pilot_graphs_lock_in_order_and_refuse_the_closing_edge` (criterion 2, deterministic interleaving). Two task-pilot applications whose proposed edges close a dependency cycle only together run on separate threads, ordered by a `#[cfg(test)]` seam at the two points of the apply lock protocol: the second discovers its graph before the first takes its locks and locks after. It asserts that neither waits out a file-lock timeout, the second rediscovers the grown graph, its edge is refused by the cycle check, and the persisted graph stays acyclic. The crate-root integration tests compile without `cfg(test)`, so they cannot reach the seam, and unforced timing would not choose this interleaving reliably [ORB-15408].

- [The plugin workspace lifecycle test](../../crates/orbit-core/src/application/plugin/tests/lifecycle/workspace.rs): `failed_workspace_enable_keeps_the_previous_disabled_toggle` (criterion 2, fault injection). It makes the routine seed destination a file, calls the workspace enable entry point, and asserts the seed error preserves a prior `false` toggle; after removing the obstacle, it verifies a successful retry seeds both definitions and writes `true`. Reusing the isolated plugin fixture avoids duplicating its install and manifest setup in a crate-root test.

- `crates/orbit-engine/src/executor/automation/ci/` attribution tests: `nextest_regions_bind_only_complete_evidence_to_the_known_failed_step` (criteria 1 and 3). This table exercises private CI attribution with streamed nextest regions and incomplete selection/command flags. Public engine fixtures cannot provide scripted CI logs to this boundary; it prevents a partial or differently attributed block from becoming actionable evidence.

- [GC leftover tests](../../crates/orbit-engine/src/executor/automation/vcs/worktree/gc/tests/leftovers.rs): `gc_keeps_trash_directory_when_deleter_pid_is_alive_and_within_timeout`, `gc_reclaims_trash_directory_when_deleter_pid_is_dead`, `gc_reclaims_trash_directory_when_age_exceeds_timeout`, and `gc_leaves_malformed_trash_directory_as_unrecognized` (criteria 1 and 4 [ORB-15702]). They verify trash-directory name parsing, age expiration against the deletion timeout, and deleter PID liveness without spawning real multi-minute deletion threads or risking disk exhaustion.

- `crates/orbit-core/src/application/plugin/tests/lifecycle/remove.rs`: `a_failed_tree_delete_keeps_the_row_for_a_retry_that_finishes_it` (criterion 2, fault injection). A `#[cfg(test)]` seam fails the next delete of the namespace install tree or of its build log, as an unlink error partway through `remove_dir_all` would. The test asserts that the row, grant witness and build witness survive the failure, and that a retried `orbit plugin remove` deletes both trees and then the row. Permission bits would not force this failure under root-run tests, and the crate-root integration tests compile without `cfg(test)`, so they cannot reach the seam.

- `crates/orbit-core/src/application/job/run/tests/owner.rs`: `provider_signalling_requires_a_verified_live_identity` (criteria 2 and 3). Injected unavailable, no-process and mismatched start-identity probes with conservative live observations must leave an owned child alive; a real matching probe is the positive teardown control. The public cancellation boundary cannot force unreadable kernel identity or the contradictory no-process-but-live outcome without changing host permissions or relying on PID reuse [ORB-15618].
- `crates/orbit-web/src/tests/ssh_tunnel.rs`: `paced_status_fragments_cannot_outlive_the_readiness_budget`, `cancel_during_a_paced_status_reaps_the_ssh_child`, and `oversized_status_line_is_refused_without_draining_the_flood` (criteria 2 and 4). A peer trickles a status line faster than the per-read socket timeout, or floods one with no newline, through the ssh stub's forward. `establish` is crate-private and `orbit web connect` blocks on signals, so the HTTP boundary cannot start the probe, cancel it, or reap the child. The tests assert the attach and spawn budgets still end the attempt, cancellation reaps the owned ssh child, and a direct no-newline flood is refused before the probe deadline. `delayed_forward_bind_attaches_without_a_second_ssh` remains the healthy-attach control. `probes_verify_status_code_over_finite_peer` (criterion 2, fault injection [ORB-15675]) drives finite one-shot loopback peers through the production `healthz_ok` and `http_get_ok_body` probes: a non-200 status is refused even when its reason phrase contains `200`, and malformed versions, signed or non-three-digit codes, and missing or nonnumeric codes are refused.
- `crates/orbit-web/src/tests/host_tunnels.rs`: `a_paced_identity_body_cannot_outlive_its_deadline` (criteria 2 and 4). After a healthy attach, the identity body trickles faster than the socket timeout. The test asserts acquire fails inside the identity budget and the owned child is reaped. The router tests cannot pace that read or observe the child.

- `crates/orbit-core/src/application/job/run/tests/owner.rs`: `owner_and_provider_never_escalate_against_a_replacement_identity` (criterion 2). A bound-target seam forces exit/reuse during verification, immediately before TERM, and during grace for both cancellation callers, with ordinary TERM/KILL controls. It captures the generation at binding and verifies that binding precedes both identity-bearing probes, even when the replacement token matches. Group preparation follows authorization, and an injected snapshot refusal must reach both callers before any signal. The runtime boundary cannot force these interleavings. `orbit-common`’s `process::signal` test `retained_signal_identity_never_targets_a_reused_numeric_pid` (criteria 2 and 4) binds a real process, reaps it, substitutes a live bystander's number, and verifies that retained Linux pidfd/macOS unique-id signals never reach the bystander. No PID exhaustion or host load is needed.
- `crates/orbit-core/src/adapter/engine_host/v2_host/pipeline_actions/tests/invoke.rs`: `an_expired_gate_wait_cancels_its_delivery_child_and_keeps_its_candidate` and `a_child_that_finishes_as_the_wait_expires_reports_its_own_outcome` (criterion 2, deterministic interleaving). An injected wait clock lets the gate's four-hour dispatch timeout pass at once and settles the child between two polls, after the wait's last look and before the parent acts on the deadline. The first asserts the expired wait cancels the running delivery child, keeps its candidate through the child's failure handoff, and returns its task to the backlog naming the timeout and that candidate; the second asserts a child that settled in that window reports its own outcome and is not cancelled. The boundary cannot place the settlement inside that window without waiting out a real deadline.

- `crates/orbit-engine/src/activity_job/cli_runner/tests/inspection.rs::inspection_git_ignores_parent_locale_and_config` and `executor/automation/vcs/tests/git.rs::host_git_classifies_failures_in_the_c_locale` (criterion 3). These private Git boundaries must discard parent locale and Git configuration authority before classifying stderr. Isolated children supply hostile parent environments; the inspection shim queries real Git's effective fsmonitor setting before executing the command, and both runners classify real failures under C and German parent locale settings. Public provider dispatch cannot expose the host runner or cheaply inject an environment observer for every inspection command.

- `crates/orbit-core/src/application/task/tests/final_recovery.rs::terminal_replay_finishes_side_effects_after_a_post_lock_read_failure` (criterion 2, fault injection). A one-shot read seam stops final recovery after its decision is durable and before resolves or PR closure. The isolated fixture verifies replay finishes done/rejected/archived effects, writes one decision, and leaves successful effects and a human-reopened friction alone on another retry. The public boundary cannot deterministically fail only that post-lock read.

- `crates/orbit-core/src/application/task/desktop/tests/write.rs::completion_replay_finishes_side_effects_after_a_post_lock_read_failure` (criterion 2, fault injection). A one-shot read seam stops a desktop completion after its receipt commits and before resolves or PR closure. The isolated fixture verifies a same-request retry resolves the target and closes the stale PR, and that another retry leaves a human-reopened friction and a recorded PR closure alone. The public boundary cannot deterministically fail only that post-lock read.
- The [worker resource-sampler tests](../../crates/orbit-core/src/application/job/pipeline/worker/tests/usage.rs): `sampler_records_one_row_per_minute_with_mean_cores_and_peak_memory`, `counter_intervals_credit_the_minutes_they_cover`, `memory_counts_shmem_but_not_page_cache` and `concurrent_workers_store_one_host_row_per_minute` (criteria 1 and 2). Fixture `cpu.stat`, `memory.current` and `memory.stat` files and a fixture `/proc` are rewritten between samples taken at injected times, so minutes of per-scope aggregation, the resume continuation, boundary-ending bursts, intervals crossing one or several minutes, and the page-cache/tmpfs split are checked in milliseconds without a systemd scope or host load. The third holds concurrent offers of one host minute at a barrier, through separate connections, and asserts exactly one insert wins. A boundary worker cannot choose its cgroup, its `/proc` or its clock.
- The [host usage reader tests](../../crates/orbit-core/src/runtime/host_resource/tests/usage.rs): `host_usage_is_tick_utilization_in_cores_and_unavailable_memory` and `malformed_proc_files_read_as_unknown` (criterion 1, parser edge cases). Fixture `/proc/stat` and `/proc/meminfo` files pin busy cores from tick deltas (not load average) and `MemTotal − MemAvailable`, and truncated, non-numeric or inconsistent files read as unknown rather than zero use. The reader is crate-private and the boundary reads the real `/proc`.
- The [delivery work-class tests](../../crates/orbit-core/src/application/job/pipeline/worker/tests/delivery_class.rs): `work_class_prefers_template_then_no_diff_then_change` (criterion 1) is the class precedence table. `class_is_captured_once_per_delivery_and_survives_tag_edits` (criteria 1 and 3) walks the admission decision table: a child inherits its delivery's class after a tag edit, a fresh submission recaptures it and overwrites a caller-supplied class, and resumes, retry runs and adopted runs are marked resumed. Reaching each row at the boundary would need a full delivery pipeline per case. `worker_without_a_scope_records_nothing_and_logs_nothing` (criterion 2, fault injection) injects an absent scope and then a fixture scope into the sampler's start and asserts no rows and no warning, then one row; the boundary cannot choose whether a worker resolves a scope.
- `crates/orbit-store/src/driver/sqlite/migration/tests/ledger.rs`: `resource_minute_tables_keep_an_older_binary_writing` (persisted format with a migration; criterion 3). It applies the ledger through the resource-minute migrations, then opens the database with the registry that ends before them, and asserts the older binary still opens it writable. No test can build an older binary.

- `crates/orbit-store/src/driver/sqlite/migration/tests/ledger.rs::store_schema_prefix_goldens` (criterion 3, persisted-schema safety): applies each shipped ledger prefix independently and compares its normalized SQLite schema with a lossless per-version golden. The public store-open boundary always applies the latest registry and cannot exercise historical prefixes without widening production visibility. The legacy recreation and actor data backfill cases use the public boundary in `tests/migration_freeze.rs`.
- `crates/orbit-store/src/driver/sqlite/migration/tests/ledger.rs::task_registry_schema_prefix_goldens` (criterion 3, persisted-schema safety): the same per-prefix golden for the task registry's own ledger (`task_registry/tests/registry_schema.golden`), a separate SQLite file the store-open boundary never builds. Adoption, the `user_version` fence and newer-registry decisions use the public registry boundary in `tests/task_registry_ledger.rs`.

- `crates/orbit-config/src/tests/machine.rs::host_shutdown_hold_minutes_defaults_and_rejects_non_positive_values` (criterion 3, fail-closed crafted input). Zero is rejected by the resolver with a message that names `machine.host_shutdown_hold_minutes`; a negative fails the integer read and names the same key. The default 180 and an uncapped 10080 are controls in the same test. The load helpers that write a global file and refuse a workspace `[machine]` table are `pub(super)` in this crate's existing machine tests, so the check stays beside them [ORB-15796].
- `the_hold_includes_the_horizon_and_a_later_instant_admits` (criterion 1), beside `crates/orbit-core/src/runtime/host_signal.rs`. One table pins the horizon arithmetic: equality holds, one second later admits, and a past timestamp holds. The pull and local drain tests, and the sweep test below, place the schedule with `Utc::now()` at minute granularity, so they cannot pin the one-second boundary without a race [ORB-15796].
- `crates/orbit-core/src/application/routines/tests/sweep.rs::a_shutdown_inside_the_hold_horizon_skips_the_sweep_and_one_beyond_it_fires` (criterion 2). The public `test_support::run_sweep_at` hardcodes the default host-signal probe, so a crate-root test cannot place a shutdown on either side of the horizon. The test drives `pub(crate)` `run_sweep_at_with_providers_at` with an injected clock and a 30-minute horizon, and asserts the tick skips at the horizon and fires one minute past it. A hardcoded 180 would still hold that far side [ORB-15796].

- `schema` in `crates/orbit-core/src/runtime/recovery_authority/tests/mod.rs` (criterion 3, persisted-authority safety): exercises adoption from a frozen pre-ledger SQL fixture across the shipped certificate and optional binding shapes, preserving each existing SQLite table definition and row, and checks newer-schema writer/read-only/refusal decisions and WAL sidecar retention after those opens close. Host authority is private and cannot be seeded or opened directly through the public leaf runtime API without weakening its trust boundary. Mutable cases re-execute in isolated children.

## Goldens

- A golden's fixture input is real or realistic, checked in, and small.
- `make goldens UPDATE=1` regenerates goldens. The PR explains every golden diff. Re-blessing a diff without explaining it is a review blocker, and so is blessing a diff to get CI green.
- A golden asserts a contract. Do not golden prose that is expected to change, such as prompts or UI copy (see the text-matching rule in `CLAUDE.md`).

## Integration-test cost

Cargo links each top-level `crates/<crate>/tests/<name>.rs` file and each `crates/<crate>/tests/<area>/main.rs` as its own binary, and every binary pays the link cost. Integration tests are therefore grouped by area: `tests/<area>/main.rs` declares one module per surface, `tests/<area>/<surface>.rs` (for example `orbit-cli`'s `output`, `mcp`, `tool`, `task`, `workspace` and `process` binaries). To keep that cost down:

- Add a new case to an existing area binary, or a new module to it for a new surface.
- Start a new binary only for a genuinely separate area, or for a test that must own its process because it installs signal handlers or mutates the environment outside `orbit_common::test_env`'s lock. Say why in its header.
- A test that re-runs itself as a child (`current_exe()` with `--exact <name>`) must pass the module-qualified name, run the child through `orbit_common::test_env::run_child_test` (a load-tolerant hang guard that reports the child's output on overrun), and verify execution with `orbit_common::test_env::assert_child_test_passed`, which rejects libtest's successful zero-test exit. For children intentionally killed or kept alive, use `assert_child_test_exists` before spawning and verify the child's readiness sentinel or handshake. Select ignored child entry points with `--ignored`.
- Fixtures that mutate Orbit state still run in an isolated child process ([DEVELOPMENT.md](../DEVELOPMENT.md#safe-mutable-cli-fixtures)).
- Waits and process guards follow STD-03 §R17 to §R20. Size them for a loaded host, not an idle one ([DEVELOPMENT.md](../DEVELOPMENT.md#fixtures-on-a-loaded-host)).
- Shell fixtures must not poll markers by forking `sleep` in `while`/`until` loops. Use a FIFO/pipe handshake or an in-process wait with a deadline. `make ci-fast` scans Rust strings in test paths, including scripts assembled across literals. A necessary bounded exception belongs in `test-shell-waits-allowlist.json` in the scripts directory with its path, exact loop header and a reason; unused exceptions fail the guard.
- A refusal fixture asserts the exact authorization cause, such as the refusal kind and the tool, grant or ceiling it names. A bare failure lets a bootstrap or sandbox error pass as the policy refusal.

- `orbit-engine`'s `claimed_base_ref_preserves_valid_refs_and_propagates_invalid_modes` (criterion 1, combinatorial pure ref selection) covers explicit modes, defaulting and claim ship-mode fallback together. The public claim action rejects invalid sync input during observation, before the red-base hold path, so it cannot expose the helper's former silent error-to-local fallback.

## Provider response boundary

`orbit-agent` keeps provider projection and response regressions in the existing
[`provider_invocation/response.rs`](../../crates/orbit-agent/tests/provider_invocation/response.rs)
integration module, with fixture cases grouped under `response/projection.rs`
and `response/envelope.rs`. Exported `normalize_cli_stdout`,
`project_cli_response`, `latest_assistant_message`,
`antigravity_terminal_error_diagnostic`, and `ParsedStdout` accept the same
crafted captures as the former sibling unit tests. Parser edge cases, failure
synthesis, transcript disclosure and usage rollups therefore need no private
test access or provider subprocess. Copilot prompt-echo fixtures obtain stdin
from public `Agent::invoke`; the Antigravity timeout matrix and its non-target
provider controls use exported `apply_antigravity_print_timeout` in
[`provider_limit.rs`](../../crates/orbit-agent/tests/provider_limit.rs). These
cases do not earn unit admission because the public boundary costs the same.

## Public types contract boundary

`orbit-types`' plugin, task, tool and workflow cases live in
[`tests/contracts/main.rs`](../../crates/orbit-types/tests/contracts/main.rs),
with modules for each surface in one integration binary. Publication cases
join the existing
[`tests/workspace.rs`](../../crates/orbit-types/tests/workspace.rs) binary.
The admission audit's 40 moved cases use the same inputs and assertions through
exported helpers and types, including the vendored provider-resolution fixture;
none needs a provider process or production visibility change.

The two retired filesystem cases are covered by `orbit-policy`'s
`tests::matrix::precedence_matrix` (later exclusion and allowed sibling) and
`tests::matrix::newline_segment_request_matrix` (single-star grant followed by
recursive deny, newline/ordinary siblings and positive recursive newline grants).
The retired `only_a_won_start_grants_execution_authority` case merely enumerated
`JobRunStartOutcome::owns_execution`, a getter excluded by this strategy.
`orbit-store`'s `allocation_admission::removed_job_run_ids_are_never_reallocated`
asserts a real store Start owns execution through its `finish` helper; it is
not an exact duplicate of all three getter variants.

The later `workflow/review/tests/gate_note.rs` admission was outside this audit
slice and remains in its sibling unit location and baseline.

## Retiring a unit test

You may delete a unit test when one of these holds:

- An integration test or golden now covers its behaviour. The deletion PR names that replacement.
- It fails the admission criteria and guards nothing the boundary misses.

Before deleting a test, check whether it is the only coverage of a security or incident guard. If it is, either move that guard to the boundary in the same change or keep the test. Keep the support code a kept test depends on: shared helpers, `#[cfg(test)]` seams in production code, and `#[ignore]` child-process entry points that a kept test re-execs.

## When NOT to

- **Do not pad a boundary test to reach an edge case it cannot express cleanly.** That case meets criterion 1 or 2, so write a small unit test.
- **Do not add e2e tests for every branch.** E2E covers critical flows. Branches belong in integration tests or table-driven unit tests.
- **Do not reach around this rule by widening visibility** so that an integration test can call internals. Test the public surface, or admit a unit test under the criteria.

## CLI admission audit reconciliation

The CLI is a binary crate, so `pub` inside its command modules is not an
exported library boundary. The 2026-10-12 audit cleanup moves plugin consent,
secret argv refusal, filesystem diagnostics, redirected-discovery teardown,
non-terminal rendering and audit deduplication to the existing crate-root
`tool`, `output`, `workspace` and `task` binaries. Secret argv forms share the
existing leak-scan invariant rather than a second parser-only test. The four
retired Linux package/bundle tests pinned selection, call order or diagnostic
wiring; the Host refusal, signed-digest and custom-profile controls remain.

Three proposed moves require private inputs that the built CLI cannot supply
without widening production access or changing the host:

- `command/init/tests/linux_host.rs::apparmor_remedy_requires_ubuntu_and_the_exact_uid_map_denial`
  (criteria 2 and 3): `FixtureHost` injects OS identity, probe denials and
  privileged operations into private `prepare_with`. Its negative cases prove
  that derivatives and different denial signatures neither elevate nor stage
  a bundle nor install a profile. The CLI reads real host OS/kernel state;
  the namespace-denial integration fixture cannot emulate Ubuntu's uid-map
  denial and successful privileged profile installation safely.
- `command/task/tests/flow.rs::rejection_then_reopen_preserves_dropped_event_and_historical_boundaries`
  (criterion 1): private `compute_flow` accepts fixed timestamps and an explicit
  `now`, proving dropped/reopened arithmetic and the intervening closed bucket
  together. `task flow` always reads `Utc::now()` and persisted history. Retain
  the pure fixed-clock case instead of adding database/history rewriting and
  a wall-clock-dependent fixture just to reach the same arithmetic.
- `command/clock/tests/tick.rs::deadline_failure_returns_nonzero_with_deferred_workspaces_in_both_views`
  (criterion 2): private `ClockTickArgs::output` accepts a constructed partial
  `SweepOutcome`, preserving both reports and the failure status together.
  The CLI constructs default `SweepOptions` and exposes no injected deadline
  or outcome. Forcing expiration at a chosen workspace through the binary
  would require waiting out the real budget and holding scheduler work;
  the existing root/alias CLI tests cannot produce that partial outcome.

These are corrected admission decisions based on the current qualifying
seams, rather than claims that the proposed moves already happened. All 15
originally admitted CLI tests and their fixtures remain intact.

The simulated `tests/audit_middleware.rs::invalid_json_bail_before_runtime_yields_exactly_one_row`
is redundant with `tests/task/audit_cli.rs::audit_cli_missing_log_records_the_render_failure`:
that existing built-CLI case proves guard-side failure writes exactly one row,
with the failure status, exit code and diagnostic. The old unit case supplied
an `OrbitError::InvalidInput` after manually creating a guard; it never parsed
JSON. Current `ToolRunArgs::bootstrap_route` returns unusable input before
runtime initialization and before an `AuditGuard` exists. The built-CLI
`tool_run_unusable_input_does_not_migrate_a_pending_layout` case proves that
refusal precedes migration. Retire the simulated duplicate rather than change
this deliberate bootstrap behavior to satisfy its stale scenario.

## Command-layer admission audit reconciliation

`orbit-cmd` keeps no crate-root `tests/`, so the 2026-10-12 audit cleanup
moves its boundary cases into the CLI crate's existing integration binaries.
Those binaries link `orbit_cmd` and call its exported entry points directly,
or run the built CLI where it already sets up the state:

- `process` (`update::flow`, `update::bundled_bwrap`,
  `update::local_candidate_inputs`) calls `run_update`, `stage_bundled_bwrap`,
  `executable_target` and `CandidateManifest::parse` against a shell-script
  installation and a mirror signed with a throwaway key
  (`update::release_fixture`). The oversized-input case makes each input a
  sparse file larger than every limit instead of one byte past each private
  limit, so it does not pin the limit values.
- `workspace` (`agent_rules`, `layout_version`, `doctor_runtime`) calls
  `inject_agent_rules`, `OrbitRuntime::from_roots` with `migrate_dry_run_at`,
  the `task-relations` probe from `doctor_report_probes`, and the graph and
  stale-lock repairs on `DoctorCommands`.
- `task` (`task_registry_recovery`) calls `inspect_task_store_partitions` and
  `remove_unclaimed_task_stores` after the built-CLI fixture loses the task
  registry. The workspace catalog now claims a registered checkout's
  partition, so the unclaimed case first runs `orbit workspace remove`, which
  keeps the partition and its bundles.

Four moves already had an exact built-CLI duplicate, so the source test was
retired instead:

- `update/tests/flow.rs::a_tampered_archive_fails_the_checksum_and_never_reaches_the_install_path`,
  `an_archive_carrying_more_than_the_orbit_binary_is_rejected` and
  `a_second_concurrent_update_is_refused_rather_than_queued` duplicate
  `process/update/integrity.rs`'s
  `a_checksum_mismatch_never_reaches_the_install_path_and_staging_is_cleaned`,
  `an_archive_with_extra_members_including_traversal_is_rejected` and
  `a_second_concurrent_update_is_refused`. Each built-CLI case asserts the same
  refusal, an untouched install and no staging file.
- `doctor/tests/permissions.rs::ownership_scan_never_visits_or_reports_worktree_or_target_contents`
  duplicates `output/doctor.rs::permission_remediation_repairs_legacy_state_and_preserves_excluded_trees`.
  That case leaves writable worktree and `target` trees under both roots. The
  permission row turns `ok` after the repair, and every excluded directory
  stays `0777` through the first and a repeated repair. The ignored
  `default_doctor_large_store_and_worktrees_under_five_seconds` measures the
  walk cost of 10,000 `target` directories.

The three retirements consolidate rather than drop cases. The off-macOS and
no-Claude skip controls join
`doctor/tests/worker_token.rs::warns_for_each_place_the_worker_token_can_be_lost_and_is_ok_when_none_is`.
The full-object-id commit forms join the relocated
`manifests_are_v1_operator_attested_documents_only`, because the manifest's
`source_commit` passes through the same normalizer. The worktree-reclaim
command guard (dk-server-2, 2026-10-09) now lives in
`workspace/worktree_gc_routing.rs::reclaim_dry_run_reports_pattern_bytes_doctor_warns_and_confirm_preserves_the_checkout`.
That test runs the confirming command doctor prints, including its
environment prefix, instead of supplying the operator override itself.

The 15 admitted `orbit-cmd` tests remain. Their update fixture is now trimmed
to the restore-callback fault injection, and the stale-lock holder helpers
sit beside the one test that uses them.

## Common-crate admission audit reconciliation

The 2026-10-12 audit's `orbit-common` moves run in the crate-root
`mechanisms` binary. It holds one module per responsibility (`fs`,
`governance`, `migration`, `observability`, `process`, `security`, `sqlite`,
`storage`) and calls the same exported helpers the sibling tests called. The
SQLite observation fixture keeps its re-executed writer child, now named
`sqlite::read_only_observation::external_writer_process`.

Two cases reach their guarantee through another public caller, because the
helper they called is crate-private:

- `private_open_repairs_a_permissive_existing_file` opens a permissive
  existing lock file through `try_acquire_exclusive_file_lock`. Lock files use
  the same create, read and write private open the case used.
- `private_append_rejects_a_final_symlink` joins `jsonl_exit_flush`, because
  the JSONL feed is the only public caller of the private append. The binary's
  existing child logs a warning with a symlink planted at the feed path, and
  the case asserts that the link's target is unchanged and the link remains.

`bounded_run_times_out_and_reaps_the_process_group` was retired as a
duplicate. `git_deadline`'s `hung_git_times_out_and_leaves_no_child` runs the
same stalled leader-and-`sleep` script through `run_bounded_capped`. It
asserts the timeout error and its `timeout_ms`, and that both pids are gone.
`process_group_liveness`'s
`bounded_timeout_reaps_a_child_that_moves_into_the_callers_group` asserts the
lower deadline bound, the reaped leader and an exited group on every Unix.

Five audited moves stay unit tests, with corrected rationale:

- `fs/generation/tests/handoff.rs::a_completed_refusal_is_not_probed_again`.
  The public `handover_target` keys its process-wide cache on the replaced
  image of the running test binary. Only the `judge_candidate` seam takes a
  caller's cache and probe timeout (see the handoff entry above).
- `process/tests/signal.rs::retained_signal_identity_never_targets_a_reused_numeric_pid`.
  It substitutes a bystander's number for a bound process's private pid to
  simulate reuse, which no public caller can do (see the run-owner entry
  above).
- The three `bounded_run_*` cases admitted above, whose leak checks and
  watchdog are the argv-identity harness the other admitted `bounded.rs`
  tests guard.

## Search-crate admission audit reconciliation

The two `orbit-search` LexicalStore cases live in the crate-root
[`lexical` integration suite](../../crates/orbit-search/tests/lexical/main.rs).
It uses `LexicalIndex` and the exported BM25 helpers to prove legacy-schema
migration preserves search and can reopen the migrated store, and to prove
task indexing, updates, deletion, and full reindexing keep FTS synchronous.
The migration fixture inspects its temporary SQLite file to retain the
existing obsolete-table and space-reclamation assertions. This public helper
boundary costs no extra process or test-only visibility, so the source tests
do not need a private `LexicalStore` or migration seam.
