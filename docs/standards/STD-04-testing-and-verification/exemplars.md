# Exemplars — STD-04 Testing and verification

Worked examples of the rules in [STD-04](STD-04.md), as pointers into the Orbit
codebase. Orbit is the example, not the audience: this file is optional reading, and
useful only with an Orbit checkout.

Paths are relative to `codebases/orbit/`.

- R1 — `crates/orbit-cli/tests/mcp_roundtrip.rs#readonly_state_mount_keeps_cli_and_mcp_reads_observational`
  (drives "the production CLI and MCP entry points" inside a real read-only mount, "rather
  than approximating the boundary with permission bits");
  `docs/design/operations-as-data/4_decisions.md` (the ORB-12582 entry, around line 271).
- R2 — `crates/orbit-types/src/workflow/review.rs#ValidationRole` (`ExpectedFailure`: "the
  pre-fix reproduction … The failure is the positive evidence");
  `crates/orbit-core/src/application/review/gate/tests/judgement.rs#classifications_that_contradict_their_outcome_or_explain_nothing_are_refused`;
  `scripts/cross-revision-check.sh` (the same command on two revisions).
- R3 — `docs/design/activity-job/4_decisions.md#Derive the delivery execution summary from the change, not from the agent`
  (Rejected alternatives); `docs/DEVELOPMENT.md#Tests that depend on host process visibility`
  ("Never weaken the assertion").
- R4 — `crates/orbit-engine/src/activity_job/cli_runner/tests/orchestrator_worktree.rs`
  (widened acceptance cases such as
  `concurrent_primary_fast_forward_does_not_block_disjoint_worktree_changes` beside the
  `expect_err("… must remain fail closed")` cases);
  `crates/orbit-policy/src/tests/matrix.rs` (a table-driven allow/deny matrix that records
  surprising behaviour "instead of silently changing enforcement semantics").
- R5 — `AGENTS.md#Rules` (the text-matching ban and its structural-guard exception) and
  `#Code` (invariants, not policy);
  `docs/design-patterns/test_layout.md#What tests assert (invariants vs. policy)`.
- R23 — `docs/design-patterns/test_strategy.md` (the layers and the admission criteria; from
  constellation-works/orbit#3015); `crates/orbit-policy/src/tests/matrix.rs#precedence_matrix`
  (an admitted table-driven unit test); `crates/orbit-cli/tests/output_goldens/` (goldens for
  output contracts).
- R24 — `docs/design-patterns/test_strategy.md#Goldens`; `scripts/check-goldens.sh`.
- R25 — `docs/design-patterns/test_strategy.md#Retiring a unit test`;
  `crates/orbit-store/src/fs/lock/tests/file_lock.rs#sigkilled_holder_releases_lock`, which depends on the
  `#[ignore]`d `crash_holder_child` entry point that it re-execs.
- R6 — `crates/orbit-engine/src/activity_job/tests/workspace.rs#GitShim` (`install`
  re-runs the test in a child with `PATH` set on the child command);
  `crates/orbit-common/src/test_env.rs#ScopedEnv`; `.config/nextest.toml` (the
  `stateful-runtime` group and its comment about keeping filters narrow).
- R7 — `crates/orbit-web/src/api/tests/routines.rs#with_clock_status` (an injected clock
  observer instead of the live user manager); `scripts/cross-revision-check.sh`
  (`GIT_CEILING_DIRECTORIES=$WORKDIR`).
- R8 — `crates/orbit-tools/src/plugin/tests/mcp.rs`
  (`#[ignore = "requires a host plugin sandbox; the Linux CI sandbox gate runs it"]`);
  `.github/workflows/ci.yml` (the `Linux sandbox/policy enforcement gate` step,
  `--run-ignored only`); `crates/orbit-common/src/test_env.rs#start_identity_probe_blocker`
  (a skip that names its constraint).
- R9 — `.config/nextest.toml` (no `retries` configured); `docs/DEVELOPMENT.md#Tests that depend on host process visibility`
  (assert the fail-safe branch instead of failing for an unrelated reason).
- R10 — `scripts/check-ci-macos.sh` ("filtered cargo test matched zero tests");
  `scripts/ci-guardrails.sh` (fails fast when `rg` is missing, because "empty search results
  read as clean" [ORB-10021]); `scripts/check-orphan-modules.sh` (F2026-08-108);
  `scripts/check-workflow-action-pins.sh` (ORB-12452).
- R11 — `Makefile#goldens` and `scripts/check-goldens.sh` (verify, or regenerate with
  `UPDATE=1`); `AGENTS.md#Gates`.
- R12 — `Makefile#ci-fast` (`ci-guardrails.sh --fast`); `scripts/ci-guardrails.sh` (every
  `--check` parity script, such as `generate-doc-indexes.sh --check` and
  `sync-plugin-skills.sh --check`, runs in both modes; fast mode skips only the workflow test
  listing, clippy, tests, doctests, rustdoc and cargo-deny).
- R13 — `docs/design/policy-sandbox/1_overview.md#2.5 Exec supervision is not default OS isolation`
  (states what is *not* guaranteed); `AGENTS.md#Rules` ("Stale docs are a review blocker").
- R14 — `scripts/ci-guardrails.sh` (`cargo test --no-fail-fast --workspace --doc`);
  `scripts/check-crate-agent-guides.sh` (every relative link in a crate guide resolves).
- R15 — `crates/orbit-types/src/workflow/review.rs#ValidationOutcome` (`passed`, `failed`,
  `denied`, `not_run`); `scripts/cross-revision-check.sh` (status captured "from a redirect
  before any bounded display", F2026-09-077); `.github/workflows/ci.yml` ("if this step is
  missing, the gate did not pass").
- R16 — `AGENTS.md#Code` (the last bullet);
  `crates/orbit-core/src/application/review/gate/tests/judgement.rs#inconsistent_claims_and_denied_validation_are_downgraded_honestly`.
- R17 — `crates/orbit-core/assets/activities/agent_implement.yaml` (validation item 7:
  "reproduce it once on the unmodified baseline"); `scripts/cross-revision-check.sh`.
- R18 — `crates/orbit-core/assets/auto_tasks/code-review.yaml` (§File confirmed findings,
  line 72); `crates/orbit-core/assets/skills/orbit/references/task-execution.md#Step 4 — Implement and validate`
  (already-landed work needs covering evidence); `AGENTS.md#Rules` ("Judge from current code,
  tests, and requirements").
- R19 — `crates/orbit-core/assets/activities/agent_implement.yaml` (item 11: re-read the
  durable `context_files` after any update, and take a final inventory).
- R20 — review-only in Orbit; F2026-08-002 and F2026-09-158 are the counter-examples.
- R21 — `crates/orbit-core/assets/activities/agent_implement.yaml` (item 3: record the starting
  HEAD and a full starting inventory); `docs/design-patterns/command.md#Destructive CLI confirmation`
  (bulk cleanup defaults to a dry run).
- R22 — `crates/orbit-core/assets/skills/orbit/references/task-execution.md#Step 4 — Implement and validate`
  ("never use positional `git stash` / `git stash pop`");
  `crates/orbit-core/assets/activities/agent_implement.yaml` (item 10: "Preserve pre-existing
  contents, another actor's edits").
