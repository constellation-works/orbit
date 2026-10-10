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

- `crates/orbit-cli/src/tests/main.rs`: `every_assembled_leaf_accepts_json_and_lists_domain_input_exclusions` (criterion 1). It walks the assembled command tree, including hidden and plugin-derived leaves that help goldens cannot enumerate, and asserts that every leaf accepts `--json`. The only exclusion is a plugin tool input named `json`.
- `crates/orbit-cmd/src/tests/agent_rules.rs`: `bad_markers_in_one_guide_leave_every_guide_unchanged` (criterion 3). It guards the no-partial-write invariant for injected agent guides: when any target has unbalanced or misordered markers, `inject_agent_rules` writes none of them. Reaching this through `orbit workspace init` would need a full workspace fixture per case, and the write ordering is decided inside `inject_agent_rules`.
- `crates/orbit-core/src/runtime/host_resource/tests/platform.rs`: `missing_descendant_reports_its_existing_ancestor_filesystem` and `relative_path_is_refused_instead_of_resolved_against_the_working_directory` (criteria 3 and 4). The disk probe canonicalizes the nearest existing ancestor before statting it, and refuses relative paths so it never resolves against the process working directory. Platform statvfs on a missing worktrees directory has no integration test on any platform.

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
