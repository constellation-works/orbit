# Orbit — agent guide

Loaded as both `AGENTS.md` and `CLAUDE.md`.

## Rules

- Work only on authorized scope. In a managed activity, leave commits and delivery transitions to the pipeline. Neither implementation nor a PR request authorizes merging.
- Don't invent task IDs — get them from `orbit.task.add`. Don't edit task files directly — use `orbit.task.update`.
- Don't add cross-crate dependencies without updating [`ARCHITECTURE.md`](ARCHITECTURE.md).
- Historical ADRs are not an authority; don't cite them. Judge from current code, tests, and requirements.
- Don't touch `CHANGELOG.md` during tasks; it is compiled at release time ([`RELEASING.md`](RELEASING.md)).
- Update affected docs in the same PR as the code. Stale docs are a review blocker.
- Every test must exercise behavior. Don't write text-matching tests — `include_str!` plus `contains()` over source, assets, or UI copy pins wording, breaks on every edit, and proves nothing runs. The only exception is a narrow structural safety guard (CSP, sanitizer wrapper, no raw `innerHTML`) whose assertion message names what it protects.
- The complete **Check / Clippy / Test** CI job has a hard budget of **under 15 minutes of elapsed wall time**. Maximize coverage of critical Orbit behaviors, security boundaries and recovery paths within that budget. Every test must protect a named observable guarantee and earn its runtime; test count alone is not a quality or speed target. Follow [test_strategy.md](docs/design-patterns/test_strategy.md) for coverage priorities, retirement evidence and timing measurements.

## Branching

- `main` — release branch; only release merges and hotfixes.
- `agent-main` — dev integration; every task PR targets it.
- Promotion and hotfix flow: [`RELEASING.md`](RELEASING.md).

## Gates

For tasks that deliver a diff, `make ci-fast`, `make ci-test-affected`, `make ci-lint`, and `make goldens` must pass before a task moves to `review`. In a managed delivery, the pipeline's final validation runs the commands the envelope lists in `task.final_validation_commands` on the final candidate; leave those to it and run the rest. `make ci-fast` checks formatting and repository guardrails; it runs no Rust tests. `make ci-test-affected` runs full test targets for changed crates and their transitive workspace dependents; focused test filters do not replace it. See [DEVELOPMENT.md](docs/DEVELOPMENT.md#validation-and-ci) for base selection and the required-command policy. `make goldens UPDATE=1` regenerates CLI help / MCP snapshot and sandbox profile goldens after an intentional surface or policy change — review the diff. Full `make ci` runs in CI on every PR; don't run it per task.

Tasks tagged `no-diff-expected` run no workspace gates or required validation commands and attach no `no-diff.json` or workspace validation logs, in owner or claimed mode. Run only commands their own brief requires: QA sweeps may build and run the product, and reviews may reproduce findings when the brief asks for it.

Run Cargo-based gates (`ci-fast`, `ci-test-affected`, `goldens`, `ci-lint`, and `cargo test` / `nextest`) sequentially when they share a target directory. If they need to overlap, give them separate `CARGO_TARGET_DIR` values. Running two gates concurrently in one target can replace `target/debug/orbit` while generation-bound fixtures are active, causing spurious CLI fixture failures (F2026-10-118).

## Code

- Skill documentation is canonical under `crates/orbit-core/assets/skills/`; edit it there, then run `scripts/sync-plugin-skills.sh` to regenerate the `plugin/skills/` mirror.
- Layering and scoping: [`ARCHITECTURE.md`](ARCHITECTURE.md). Reusable patterns: [`docs/design-patterns/`](docs/design-patterns/).
- Lints are configured via `[workspace.lints]`: `unwrap`/`expect` are warned on in production code throughout each crate (tests are exempted at crate roots); propagate errors, using `OrbitError` at crate boundaries. No `print!` (use `tracing`), no lock guards across `.await`.
- `missing_docs` is a workspace warning that crate roots may allow while documentation is incomplete. It is currently enforced in `orbit-config` and `orbit-web`; the other crate roots opt out.
- Default to `pub(crate)`; workspace deps via `.workspace = true`; bounded channels; typed `thiserror` variants.
- Choose validation in proportion to the affected behavior and risk. Use meaningful manual QA through the real CLI, MCP, API or UI when it can reveal relevant defects, including appropriate failure paths, and record the steps, expected results, actual results and evidence for those checks at handoff. Documentation-only, mechanical or otherwise adequately covered changes may omit manual QA with a brief reason; meaningful automated evidence may suffice. Report unavailable capabilities honestly rather than as passing checks. This guidance does not replace required delivery gates.
- For an observed, reproduced defect, add a focused regression check at a public CLI/API boundary that fails before the fix and passes afterward. Do not write tests that directly target private implementation functions. Private fixtures and scenario helpers may drive public-boundary behavior; they are not private-function tests. Do not add speculative unit coverage for features, refactors or chores, including merely to satisfy a test-count or coverage target. This defect-driven policy supersedes broader unit-test admission guidance in the linked test strategy.
- Preserve critical coverage and required delivery gates. Consolidate or retire redundant and low-signal cases through measured, scoped changes that name the surviving behavioral coverage. Never remove the only guard of a security invariant, recovery path or known incident. Fix slow fixtures and flaky tests at their cause; retries, hidden skips, weaker assertions and omitted required checks do not satisfy the 15-minute budget.
- Admitted unit tests live in a sibling `tests/` dir mirroring source filenames ([`test_layout.md`](docs/design-patterns/test_layout.md)); crate-root `tests/` is integration only.
- Before writing a test, weigh what it guards and what it costs: assert what the code guarantees (parses, required fields present, structural safeguards), not policy that lives in config or prompts — crew, model, schedule, complexity, prose wording. A test that pins those turns every ops edit into a red CI; if a pin is truly warranted, cite the incident it guards in the assertion message.
- Never expose internal task/friction IDs in user-facing output, CLI help (Clap renders `///`), or MCP text.
- Fixtures that mutate Orbit state must run in an isolated child process — see [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md#safe-mutable-cli-fixtures).
- Prefer the fewest moving parts: delete dead code and stale docs together, keep compatibility only for an external contract or persisted format.
- File length is a heuristic, not a limit: past ~800 lines, check whether a file holds more than one responsibility and split along those if so; keep closely related functionality together when splitting would only scatter it.
- Report commands and outcomes at handoff — passed, failed, not run — never "tested".

## Local Rust CodeQL checks

Hosted CodeQL (`.github/workflows/codeql.yml`) analyses every pull request and every `agent-main` push. A delivery therefore does not run CodeQL locally, and doesn't record it as owed, unless one of these applies:

- the task repairs a code-scanning alert or another CodeQL finding;
- the task is tagged `security` or `security-review`;
- the change edits the CodeQL setup: `.github/codeql/**` or `scripts/codeql-rust-local.sh`.

When one applies, follow the [local CodeQL runbook](docs/runbooks/codeql-local.md), which uses `scripts/codeql-rust-local.sh`.

- Its extraction checks are required evidence. A query result from incomplete semantic extraction cannot confirm a repair.
- Run it once per delivery, against the final candidate, in the foreground with a timeout of at least 45 minutes, and read that run's `results.sarif`. An interrupted run starts over.
- Only a Linux host can extract every module. Elsewhere the script exits 3 without running, and the check is owed by a Linux run recorded as `codeql` external evidence (runbook, Non-Linux hosts).

For any other change, hosted CodeQL on the pull request is the check, and an alert it raises after merge becomes a repair task.

## Orbit Workflow

For any Orbit lifecycle work, invoke the `orbit` skill; its `SKILL.md` routes to the matching reference.
