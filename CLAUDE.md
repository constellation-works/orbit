# Orbit — agent guide

Loaded as both `AGENTS.md` and `CLAUDE.md`.

## Rules

- Work only on authorized scope. In a managed activity, leave commits and delivery transitions to the pipeline. Neither implementation nor a PR request authorizes merging.
- Don't invent task IDs — get them from `orbit.task.add`. Don't edit task files directly — use `orbit.task.update`.
- Don't add cross-crate dependencies without updating [`ARCHITECTURE.md`](ARCHITECTURE.md).
- Historical ADRs are not an authority; don't cite them. Judge from current code, tests, and requirements.
- Don't touch `CHANGELOG.md` during tasks; it is compiled at release time ([`RELEASING.md`](RELEASING.md)).
- Update affected docs in the same PR as the code. Stale docs are a review blocker.

## Branching

- `main` — release branch; only release merges and hotfixes.
- `agent-main` — dev integration; every task PR targets it.
- Promotion and hotfix flow: [`RELEASING.md`](RELEASING.md).

## Gates

`make ci-fast`, `make ci-lint`, and `make goldens` must pass before a task moves to `review`. `make goldens UPDATE=1` regenerates CLI help / MCP snapshot goldens after an intentional surface change — review the diff. Full `make ci` runs in CI on every PR; don't run it per task.

## Code

- Layering and scoping: [`ARCHITECTURE.md`](ARCHITECTURE.md). Reusable patterns: [`docs/design-patterns/`](docs/design-patterns/).
- Lints are enforced via `[workspace.lints]`: no `unwrap`/`expect` at crate boundaries (propagate `OrbitError`), no `print!` (use `tracing`), no lock guards across `.await`.
- Default to `pub(crate)`; workspace deps via `.workspace = true`; bounded channels; typed `thiserror` variants.
- Unit tests live in a sibling `tests/` dir mirroring source filenames ([`test_layout.md`](docs/design-patterns/test_layout.md)); crate-root `tests/` is integration only.
- Never expose internal task/friction IDs in user-facing output, CLI help (Clap renders `///`), or MCP text.
- Fixtures that mutate Orbit state must run in an isolated child process — see [`CONTRIBUTING.md`](CONTRIBUTING.md#safe-mutable-cli-fixtures).
- Prefer the fewest moving parts: delete dead code and stale docs together, keep compatibility only for an external contract or persisted format, ~800 lines per file is a split signal.
- Report commands and outcomes at handoff — passed, failed, not run — never "tested".

## Orbit Workflow

For any Orbit lifecycle work, invoke the `orbit` skill; its `SKILL.md` routes to the matching reference.
