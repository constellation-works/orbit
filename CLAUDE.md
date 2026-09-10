# Orbit — agent guide

Project instructions for agents working on Orbit (loaded as both `AGENTS.md` and `CLAUDE.md`).

## Rules

- Orbit supports many kinds of work and users. Keep shared agent activities
  domain-neutral; put code-, language-, and repository-specific instructions
  in the owning workspace's AGENTS.md.

- Work only on authorized scope. In a managed implementation activity, leave
  commits and delivery transitions to the pipeline. In explicitly authorized
  direct work, commit validated, task-scoped changes and open a PR when asked.
  Neither implementation nor a PR request authorizes merging.
- **Don't invent task IDs** — get them from `orbit.task.add`. Don't edit task files directly — use `orbit.task.update`.
- **Don't add cross-crate dependencies** without checking and updating [`ARCHITECTURE.md`](ARCHITECTURE.md). If a new edge is genuinely needed, make its ownership and direction explicit in the same change.
- Historical ADRs are being retired and are not an authority. Do not search for,
  cite, or use ADRs to justify a decision. Judge from the current code, runtime
  behavior, tests, documented constraints, and the requirements at hand.

## Branching

- **`main`** is the release / production branch — only release merges and hotfixes land here. Default base for external install URLs, npm/Homebrew consumers, and the GitHub default-branch view.
- **`agent-main`** is the dev integration branch — every task PR targets `agent-main`.
- **Promotion**: each release tags on `agent-main`, then merges `agent-main → main` via a merge commit. See [`RELEASING.md`](RELEASING.md) §10b.
- **Hotfixes** branch from `main`, merge to `main`, tag a patch release on `main`, then back-merge `main → agent-main` in the same session. See [`RELEASING.md`](RELEASING.md) §Hotfix flow.

## Build / Lint

`make ci-fast` (fmt-check + guardrail scripts; no compile) and `make ci-lint` (the same workspace-wide, all-target clippy pass as CI, with warnings denied) must both pass before a task moves to `review`. Each task therefore pays for one workspace clippy compile; cold runs can take several minutes, while warm runs reuse Cargo's incremental cache. The full `make ci` is the canonical merge gate via [`.github/workflows/ci.yml`](.github/workflows/ci.yml) on every PR — don't run it per task locally.

## Mutable Fixture Operations

- Keep authorized operator/production CLI actions separate from test fixtures and ad-hoc reproductions. Normal user commands may use the configured Orbit state; a fixture that can create, update, archive, or run Orbit tasks, runs, workspaces, or registry entries must use a controlled child process.
- Give the child absolute disposable paths for its `HOME`/`USERPROFILE` and checkout or data roots. Before setting deliberate fixture values, apply the existing [`orbit_common::test_env::clear_inherited_authority`](crates/orbit-common/src/test_env.rs) helper to the `assert_cmd::Command` or `std::process::Command`; do not recreate its environment-variable list locally.
- After controlled setup and before the first task/run mutation, verify routing with a read-only command such as `orbit workspace show --format json`, asserting `checkout.repo_root` and `checkout.orbit_dir` against paths canonicalized with `fs::canonicalize` because the child reports physical checkout paths. If initialization is the behavior under test, keep initialization itself on the disposable paths and verify routing before any subsequent mutation.
- Do not run bare mutable fixture commands from a shell inside a managed worker. Exporting `HOME` alone is insufficient: the inherited managed-run marker plus `ORBIT_REGISTRY_ROOT`/`ORBIT_WORKSPACE` can carry durable authority and outrank home-directory discovery. These rules guide fixtures only; they do not add restrictions to legitimate operator or production CLI use.


## Architecture

Crate layering, per-crate responsibilities, and scoping rules live in [`ARCHITECTURE.md`](ARCHITECTURE.md). Read it before adding a new crate, a new dependency edge, or a new persisted artifact.

Reusable codebase-specific patterns (Command, RAII guard, newtype, crate-boundary error translation) live in [`docs/design-patterns/`](docs/design-patterns/). When you reach for one of those shapes, copy from the documented reference instead of inventing a new one.

## Simplicity and ownership

- Correctness, readability, and maintainability are equal parts of completion.
  Code aesthetics are not optional polish: a reader should be able to locate a
  rule, follow the normal path, and recognize the failure paths without decoding
  dense expressions or jumping through unrelated helpers.
- Separate module documentation, imports, and top-level declarations with blank
  lines. Within functions, use blank lines between logical phases; keep closely
  related statements together. Do not compress a file into an uninterrupted wall
  of code or add a blank line after every statement. Formatters do not supply
  this organization for you.
- Prefer descriptive domain names, straightforward control flow, and consistent
  ordering of related operations. Follow sound neighboring conventions so
  similar work looks similar; do not copy a confusing pattern merely for
  consistency. Name a complex condition when its meaning is clearer than its
  expression. Comments explain intent, constraints, and non-obvious tradeoffs,
  not a narration of the syntax.
- Optimize first for clarity and the fewest moving parts. Do not preserve a
  wrapper, compatibility layer, abstraction, or configuration path merely
  because it already exists. Keep compatibility only when an external contract
  or persisted format requires it, and state that constraint next to the code.
- Put each rule at its authoritative boundary. Transport and UI layers should
  collect inputs and adapt protocols; domain validation, authorization, and
  persistence invariants belong in the server/domain layer that can enforce
  them. Do not duplicate a server rule as client-side orchestration.
- Every crate and module must have one explainable job. If a dependency reads
  backwards, a file contains unrelated domains, or two crates contain the same
  helper, move the behavior to the lowest appropriate owner instead of adding
  another facade or forwarding chain.
- Treat roughly 800 lines in one source or test file as a design warning, not a
  target. Likewise, several related flat files (`task_add.rs`, `task_list.rs`,
  and so on) are a signal to create a domain module with sibling tests. Split by
  responsibility before adding more code.
- Treat a long function with several phases, policies, or failure modes as the
  same warning at function scale. Name the phases, extract cohesive helpers,
  and keep the top-level flow readable without jumping through empty wrappers.
- Prefer one canonical execution path and test it at that boundary. Thin entry
  points may attach context, but they must not grow parallel dispatch,
  validation, persistence, or audit implementations.
- Do not build speculative v2 machinery into a v1 change. Add the smallest
  complete seam the current behavior needs; introduce policy frameworks or
  generalized traits only when a concrete second use makes the boundary real.
- Delete dead application code and stale current documentation together.
  Preserve shipped migrations and durable data unless the change includes an
  explicit, tested compatibility plan.

## Design Docs

- **Layout.** Feature design docs live under `docs/design/<feature>/`. Keep current explanatory docs aligned with the implementation when they remain useful.
- **Same-PR updates.** Change affected current docs in the same PR as the code. Stale descriptions of live behavior are a review blocker.

## CHANGELOG entries

Do not modify `CHANGELOG.md` during task execution; it is compiled at release
time from merged work. See [`RELEASING.md`](RELEASING.md).

## Evidence and handoff

- Inspect the owning implementation, callers, and tests before changing code.
  Use the repository's language toolchain, package scripts, and lockfiles.
- For regressions, demonstrate that a test detects the original fault when
  feasible. Run the applicable formatter and linter before handoff.

- Test observable behavior at the boundary that owns it, including relevant
  failure and edge cases. Prefer assertions that would fail if the behavior
  regressed over source-text checks that merely prove a phrase exists.
- Exercise the actual affected environment when behavior depends on an OS,
  browser, filesystem, or external tool. A mock or a passing Linux test does not
  establish macOS behavior; state what each check does and does not prove.
- Before handoff, read the whole diff as a maintainer: check naming, logical
  grouping, consistency, error paths, stale comments, and unnecessary machinery.
  Fix readability problems in the changed code, not just formatter complaints.
- Report commands and outcomes, not just "tested." Distinguish passed, failed,
  and not run, with reasons and remaining risk. Never describe a skipped check
  as passing or silently weaken a test to obtain a green result.

## Rust Practices

- Concrete internal task, learning, and friction IDs are allowed in ordinary source comments, but never expose them in user-facing errors, CLI help/output, generated files, or advertised MCP/tool text; note that Clap renders `///` comments on commands and arguments as public help.

Lint-enforced rules (full set in `[workspace.lints]`; key implications below):

- **No `unwrap()` / `expect()` at crate boundaries.** Propagate `OrbitError`; use `expect("<invariant>")` only when the invariant is local and documented. See [`docs/design-patterns/error_translation.md`](docs/design-patterns/error_translation.md).
- **No `print!` / `eprint!`.** Use `tracing` with structured fields (`tracing::info!(run_id, ...)`), not string interpolation. Allowlisted only for genuine CLI/example user output.
- **No lock guards across `.await`.** Scope `std::sync::Mutex` / `RwLock` to a block, or use `tokio::sync` for cross-task state.

Conventions (not lint-enforced):

- Remove code made obsolete by a change instead of suppressing warnings or
  keeping an unused alternate path. Preserve required compatibility and explain
  its concrete consumer.
- Register third-party dependencies in root `[workspace.dependencies]` and
  consume them with `.workspace = true`.

- **Errors:** reach for typed `thiserror` variants over ad-hoc strings when translating into `OrbitError`.
- **Visibility:** default to `pub(crate)`; reserve `pub` for items in the crate's documented public surface (see `ARCHITECTURE.md`). Re-export at the crate root only for types genuinely part of the API.
- **Channels:** bounded channels by default.
- **Tests:** unit tests live in a *sibling* `tests/` directory mirroring source filenames (`src/command/skill.rs` → `src/command/tests/skill.rs`). The sibling layout structurally enforces public-surface testing. Crate-root `tests/` is for integration tests only. See [`docs/design-patterns/test_layout.md`](docs/design-patterns/test_layout.md). Don't introduce a new test harness when an existing one fits.

## Orbit Workflow

For any Orbit lifecycle work (creating tasks, executing, reviewing, raising PRs), invoke the `orbit` skill. Its `SKILL.md` is a router: load the reference that matches the job — `references/task-authoring.md` for authoring quality standards, `references/task-execution.md` for pickup through handoff, and so on.
