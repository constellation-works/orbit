---
type: pattern
summary: "Boundary-First Testing: integration, golden and e2e by default; unit tests by exception"
last_validated: 2026-10-09
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

- `crates/orbit-core/src/application/job/pipeline/worker/tests/supervisor.rs`: `store_read_faults_retain_supervision_and_reap_in_a_live_parent` (criterion 2, fault injection). A SQLite store wrapper injects unreadable and missing startup records and a post-exit read fault while a socket rendezvous bounds a real child. The isolated parent asserts fresh-state settlement and OS reaping before it exits; the public runtime cannot inject these observer reads.
- The [admissions-stop regressions](../../crates/orbit-core/src/application/job/run/tests/admissions_stop.rs) and `crates/orbit-core/src/application/job/run/tests/drain_cancel.rs`: initialization races (criterion 2, deterministic interleaving). One-shot seams after `NoState` force a worker's first checkpoint and child dispatch, or terminalization with and without that checkpoint, before control initialization. They verify preserved progress and lineage, a merged control for a live drain, and no new effective control for a terminal drain. Crate-root integration tests cannot force this transaction ordering [ORB-15611].
- `crates/orbit-cli/src/tests/main.rs`: `every_assembled_leaf_accepts_json_and_lists_domain_input_exclusions` (criterion 1). It walks the assembled command tree, including hidden and plugin-derived leaves that help goldens cannot enumerate, and asserts that every leaf accepts `--json`. The only exclusion is a plugin tool input named `json`.
- `crates/orbit-cmd/src/tests/agent_rules.rs`: `bad_markers_in_one_guide_leave_every_guide_unchanged` (criterion 3). It guards the no-partial-write invariant for injected agent guides: when any target has unbalanced or misordered markers, `inject_agent_rules` writes none of them. Reaching this through `orbit workspace init` would need a full workspace fixture per case, and the write ordering is decided inside `inject_agent_rules`.
- `crates/orbit-cmd/src/doctor/tests/worker_token.rs`: `clock_file_is_ready_only_when_it_holds_a_credential_the_workspace_admits` (criterion 1). It is a table over every pairing of clock-file credential names with the workspace's effective pass, asserting that the clock is ready only when an admitted credential is held, and that a held credential the pass omits is named and warned. The boundary needs an installed launchd or systemd clock to reach this classification. `absent_empty_refused_and_mismatched_clock_files_stay_distinct` (criterion 1) pins that absent, empty, refused and mismatched files stay distinct states.
- `crates/orbit-core/src/runtime/host_resource/tests/platform.rs`: `missing_descendant_reports_its_existing_ancestor_filesystem` and `relative_path_is_refused_instead_of_resolved_against_the_working_directory` (criteria 3 and 4). The disk probe canonicalizes the nearest existing ancestor before statting it, and refuses relative paths so it never resolves against the process working directory. Platform statvfs on a missing worktrees directory has no integration test on any platform.
- `crates/orbit-core/src/runtime/host_resource/tests/platform.rs`: `cpu_window_reuses_advancing_or_repeated_ticks_without_refreshing_the_baseline`, `cpu_window_missing_stale_future_or_identical_baselines_take_a_real_window`, `cpu_window_native_failures_clock_reversal_and_stalls_remain_unknown`, and `cpu_window_handles_tick_wrap_and_the_freshness_boundary` (criteria 1 and 2). Injected counters, timestamps and waits cover XNU cache plateaus, expiry endpoints, failed reads, clock jumps and counter wrap deterministically without generating host load or waiting for a real wrap. The macOS CLI integration test covers actual cross-process history reuse and cold-start recovery.
- `crates/orbit-store/src/driver/sqlite/task_registry/tests/mod.rs`: `fresh_open_through_symlinked_root_matches_reopen_by_canonical_root` (criterion 3). It guards symlink confinement of the registry root: a fresh open through a symlinked root and a later open of the canonical root must key one `workspaces_dir`, because the repair gate keys its attempt budget on that path. `workspaces_dir` is crate-private, so no public-surface test can observe the split.
- `crates/orbit-store/src/repository/task/coordination/tests/admission.rs`: `a_grant_never_takes_a_footprint_the_owner_wave_admitted_before_its_section` and `an_owner_wave_after_a_grant_refuses_the_claimed_footprint` (criterion 2, deterministic interleaving). The first admits an owner drain wave through the crate-private `after_selection` seam, between a pull grant's selection and its exclusive section, and asserts the grant neither takes the admitted task nor one overlapping its footprint. The second commits the grant first and asserts the wave's section refuses the claimed footprint even though the wave's snapshot predates the claim. At the runtime boundary the claim's task is already `in-progress` before any later drain snapshot, and nothing can land a grant between a wave's snapshot and its section, so neither ordering is reachable there.
- `crates/orbit-engine/src/executor/automation/vcs/tests/base_claim.rs`: `a_candidate_rerun_stopped_at_the_reserve_still_attributes_its_failures_to_the_base` (criterion 2, fault injection). It stops a candidate rerun that hangs after its failures are printed, using a replay budget of seconds, and asserts that settlement still replays only the printed failing tests on the base and names the cut. The boundary sets the budget in whole review minutes, so reaching the cut there would cost a quarter of a minute or more of wall time per run.
- `crates/orbit-engine/src/executor/automation/vcs/tests/base_claim.rs`: `supervisor_stops_never_classify_partial_failures_as_known_red` (criterion 2, fault injection). In an isolated child, a lowered capture ceiling forces an actual output-capture stop with only 2 KiB of output; a self-signalled run and a completed run are controls. It checks that the shared known-red predicate rejects supervisor stops and base-claim classification refuses them before replay. The settlement boundary covers the signal, but cannot reach the shared predicate after the early refusal [ORB-15590].
- `crates/orbit-common/src/fs/generation/tests/handoff.rs`: `a_probe_that_times_out_is_retried_at_the_next_boundary` and `a_completed_refusal_is_not_probed_again` (criterion 2, fault injection). A candidate's first exec can be held past the probe bound by the OS, which the boundary cannot reproduce on demand, and the process-wide refusal cache is keyed by the replaced image of the running test binary. The first injects a short timeout against a script that stalls then answers and asserts the timeout is not cached; the second asserts that wrong-contract, missing-capability and non-zero-exit answers are probed once.
- `crates/orbit-core/src/adapter/engine_host/v2_host/task_pilot/tests/persist.rs`: `overlapping_pilot_graphs_lock_in_order_and_refuse_the_closing_edge` (criterion 2, deterministic interleaving). Two task-pilot applications whose proposed edges close a dependency cycle only together run on separate threads, ordered by a `#[cfg(test)]` seam at the two points of the apply lock protocol: the second discovers its graph before the first takes its locks and locks after. It asserts that neither waits out a file-lock timeout, the second rediscovers the grown graph, its edge is refused by the cycle check, and the persisted graph stays acyclic. The crate-root integration tests compile without `cfg(test)`, so they cannot reach the seam, and unforced timing would not choose this interleaving reliably [ORB-15408].

- `crates/orbit-engine/src/executor/automation/ci/` attribution tests: `nextest_regions_bind_only_complete_evidence_to_the_known_failed_step` (criteria 1 and 3). This table exercises private CI attribution with streamed nextest regions and incomplete selection/command flags. Public engine fixtures cannot provide scripted CI logs to this boundary; it prevents a partial or differently attributed block from becoming actionable evidence.

- `crates/orbit-core/src/application/plugin/tests/lifecycle/remove.rs`: `a_failed_tree_delete_keeps_the_row_for_a_retry_that_finishes_it` (criterion 2, fault injection). A `#[cfg(test)]` seam fails the next delete of the namespace install tree or of its build log, as an unlink error partway through `remove_dir_all` would. The test asserts that the row, grant witness and build witness survive the failure, and that a retried `orbit plugin remove` deletes both trees and then the row. Permission bits would not force this failure under root-run tests, and the crate-root integration tests compile without `cfg(test)`, so they cannot reach the seam.

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

## Retiring a unit test

You may delete a unit test when one of these holds:

- An integration test or golden now covers its behaviour. The deletion PR names that replacement.
- It fails the admission criteria and guards nothing the boundary misses.

Before deleting a test, check whether it is the only coverage of a security or incident guard. If it is, either move that guard to the boundary in the same change or keep the test. Keep the support code a kept test depends on: shared helpers, `#[cfg(test)]` seams in production code, and `#[ignore]` child-process entry points that a kept test re-execs.

## When NOT to

- **Do not pad a boundary test to reach an edge case it cannot express cleanly.** That case meets criterion 1 or 2, so write a small unit test.
- **Do not add e2e tests for every branch.** E2E covers critical flows. Branches belong in integration tests or table-driven unit tests.
- **Do not reach around this rule by widening visibility** so that an integration test can call internals. Test the public surface, or admit a unit test under the criteria.
