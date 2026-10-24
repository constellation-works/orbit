# Orbit — agent guide

Loaded as both `AGENTS.md` and `CLAUDE.md`. For any Orbit lifecycle work, invoke the `orbit` skill.

## Rules

- Work only on authorized scope. In a managed activity, leave commits and delivery transitions to the pipeline. Neither implementation nor a PR request authorizes merging.
- Get task IDs from `orbit.task.add` and change tasks with `orbit.task.update`; never invent IDs or edit task files.
- Don't add cross-crate dependencies without updating [`ARCHITECTURE.md`](ARCHITECTURE.md).
- Historical ADRs are not an authority; judge from current code, tests and requirements.
- Don't touch `CHANGELOG.md`; it is compiled at release time ([`RELEASING.md`](RELEASING.md)).
- Update affected docs in the same PR as the code. Stale docs are a review blocker.
- Report commands and outcomes at handoff — passed, failed, not run — never "tested". Report unavailable capabilities honestly, not as passing checks.

## Branching

`main` is the release branch (release merges and hotfixes only). `agent-main` is the dev integration branch every task PR targets. Promotion and hotfixes: [`RELEASING.md`](RELEASING.md).

## Gates

- There is no required local gate for a diff. The required PR checks (Check / Clippy / Test and Test shards 1–3) cover formatting, lint, goldens and the full suite; a red check is repaired through the PR. Don't run `make ci`, `ci-fast`, `ci-lint`, `goldens` or `ci-test-affected` as blanket pre-handoff gates. Run what the change needs: tests of the changed behavior, Clippy on touched crates, and `make goldens` when CLI help, MCP text or another golden surface changes (`UPDATE=1` regenerates; review the diff). A managed delivery's final validation runs `task.final_validation_commands`. See [DEVELOPMENT.md](docs/DEVELOPMENT.md#validation-and-ci).
- When an edit changes a generated row of `docs/INDEX.md` (indexed frontmatter, first heading or summary), run `make docs-index` and `scripts/generate-doc-indexes.sh --check`, and commit the index change with the source.
- `no-diff-expected` tasks run no workspace gates or validation commands and attach no `no-diff.json` or validation logs; run only what the brief requires.
- `skip-validation` and `skip-review` skip local validation (including docs-index) and review only while every changed path matches `workflow.light_delivery_paths`; any other path voids both. Only filing or an operator sets them. See [CONFIG.md](docs/CONFIG.md#light-delivery-skip-review-and-skip-validation).
- Cargo gates sharing a target directory run sequentially; overlapping ones need separate `CARGO_TARGET_DIR`s, or a rebuilt `target/debug/orbit` breaks generation-bound fixtures (F2026-10-118).

## Code

- Layering and scoping: [`ARCHITECTURE.md`](ARCHITECTURE.md). Reusable patterns: [`docs/design-patterns/`](docs/design-patterns/).
- Skill docs are canonical under `crates/orbit-core/assets/skills/`; after editing, run `scripts/sync-plugin-skills.sh` to regenerate `plugin/skills/`.
- Lints live in `[workspace.lints]`: `unwrap`/`expect` warn in production code; propagate errors, with `OrbitError` at crate boundaries. No `print!` (use `tracing`), no lock guards across `.await`. `missing_docs` is enforced in `orbit-config` and `orbit-web`; other crate roots opt out.
- Default to `pub(crate)`, `.workspace = true` deps, bounded channels and typed `thiserror` variants.
- Prefer the fewest moving parts: delete dead code and stale docs together; keep compatibility only for an external contract or persisted format.
- Past ~800 lines, check whether a file holds more than one responsibility and split along those; don't scatter closely related code.
- Never expose internal task/friction IDs in user-facing output, CLI help (Clap renders `///`) or MCP text.
- User-facing CLI help and errors, MCP descriptions and UI text must stand alone: no Orbit source-tree paths or repo docs, only verified published docs or installed resources. Contributor docs may cite source paths; user-workspace paths like `.orbit/tmp/` are fine.

## Tests and QA

- Add tests only for an observed, reproduced defect: a focused regression check at a public CLI/API boundary that fails before the fix and passes after. No private-function tests (private fixtures driving public behavior are fine) and no speculative coverage for features, refactors or chores. This supersedes broader admission guidance in [test_strategy.md](docs/design-patterns/test_strategy.md), which still governs coverage priorities, retirement evidence and timing.
- Every test exercises behavior a guarantee depends on. No text-matching tests (`include_str!` + `contains()` over source, assets or UI copy), except a narrow structural safety guard (CSP, sanitizer wrapper, no raw `innerHTML`) whose message names what it protects. Don't pin config or prompt policy (crew, model, schedule, complexity, wording); if a pin is warranted, cite its incident in the assertion message.
- The Check / Clippy / Test job has a hard budget of 10 minutes wall time at p90 over PR runs (queue wait reported separately). Fix slow or flaky tests at their cause; retries, hidden skips, weaker assertions or dropped checks don't count. Retire redundant cases with measured changes that name the surviving coverage, never the only guard of a security invariant, recovery path or known incident.
- Admitted unit tests live in a sibling `tests/` dir mirroring source filenames ([`test_layout.md`](docs/design-patterns/test_layout.md)); crate-root `tests/` is integration only. Fixtures that mutate Orbit state run in an isolated child process ([DEVELOPMENT.md](docs/DEVELOPMENT.md#safe-mutable-cli-fixtures)).
- Do manual QA through the real CLI, MCP, API or UI, including failure paths, when it can reveal defects, and record steps, expected and actual results and evidence. Docs-only, mechanical or already covered changes may skip it with a reason.
- A diff that changes something users see (dashboard, web pages, docs site, desktop UI) attaches screenshots of each changed view with `orbit.task.artifact.put`, at paths like `screenshots/<view>-<theme>-<width>.png`, captured under `.orbit/tmp/` (`$ORBIT_SCRATCH_DIR`). Cover every supported theme (the dashboard is dark-only), 375px width for responsive layouts, and a base-revision before shot when practical. Stay under the 1 MiB artifact limit (viewport captures, or JPEG/WebP). List the paths at handoff; if the host has no browser kit or the view can't render, say so. Reviewers treat unexplained missing screenshots as missing evidence.

## Local Rust CodeQL

Hosted CodeQL (`.github/workflows/codeql.yml`) analyses every `agent-main` push and runs weekly, not on PRs; its alerts become repair tasks. Run it locally only when the task repairs a CodeQL finding, is tagged `security` or `security-review`, or edits `.github/codeql/**` or `scripts/codeql-rust-local.sh`. Then follow the [local CodeQL runbook](docs/runbooks/codeql-local.md):

- Extraction checks are required evidence; results from incomplete extraction can't confirm a repair.
- Run once per delivery against the final candidate, in the foreground with a timeout of at least 45 minutes, and read that run's `results.sarif`. An interrupted run starts over.
- Only Linux extracts every module. Elsewhere the script exits 3, and the check is owed by a Linux run recorded as `codeql` external evidence.
