---
type: context
summary: "Orbit agent guide: rules, branching, gates, code and tests, local CodeQL"
last_validated: 2026-11-22
---

# Orbit — agent guide

Orbit is a local-first runtime for coding agents, built as a Rust workspace. `CLAUDE.md` imports this file. For any Orbit lifecycle work, invoke the `orbit` skill.

## Rules

- Work only on authorized scope. In a managed activity, leave commits and delivery transitions to the pipeline. Neither implementation nor a PR request authorizes merging.
- Get task IDs from `orbit.task.add` and change tasks with `orbit.task.update`; never invent IDs or edit task files.
- Don't add cross-crate dependencies without updating [`ARCHITECTURE.md`](ARCHITECTURE.md).
- Historical ADRs are not an authority; judge from current code, tests and requirements.
- Don't touch `CHANGELOG.md`; it is compiled at release time ([`RELEASING.md`](RELEASING.md)).
- A breaking change by [`RELEASING.md` Versioning](RELEASING.md#versioning) gets the `breaking-change` task tag and one execution-summary line `Breaking: <x removed → use y>`. The implementer sets both and the reviewer confirms them; release tooling derives the version bump and changelog bullet from them.
- Update affected docs in the same PR as the code. Stale docs are a review blocker.
- Edit an `AGENTS.md` only when a change adds, changes or removes a rule, a command or a pointer in it; delete or rewrite a rule whose subject a change removes. A new feature is described in its own docs.
- Never list `docs/CONFIG.md` in a task's `context_files`: it is a shared reference doc, and the implementer updates it in the same PR.
- The `doc-duties` auto-task also validates the root `AGENTS.md`, `CONTRIBUTING.md`, `SECURITY.md`, `ARCHITECTURE.md` and `RELEASING.md`; keep their `last_validated` frontmatter.
- Report commands and outcomes at handoff — passed, failed, not run — never "tested". Report unavailable capabilities honestly, not as passing checks.

## Branching

`main` is the release branch (release merges and hotfixes only). `agent-main` is the dev integration branch every task PR targets. Promotion and hotfixes: [`RELEASING.md`](RELEASING.md).

## Gates

- There is no required local full-suite gate for a diff. The required PR checks (Check / Clippy / Test and Test shards 1–3) cover formatting, lint, goldens and the full suite; a red check is repaired through the PR. Don't run `make ci`, `ci-fast`, `ci-lint`, `goldens` or `ci-test-affected` as blanket pre-handoff gates. Run what the change needs: tests of the changed behavior and `make goldens` when CLI help, MCP text or another golden surface changes (`UPDATE=1` regenerates; review the diff). A managed delivery's final validation runs `task.final_validation_commands`. See [DEVELOPMENT.md](docs/DEVELOPMENT.md#validation-and-ci).
- Anchor focused `cargo test -p <crate> …` commands with `-p orbit-cli` so they resolve one workspace feature set and reuse the cached build.
- Pre-handoff checks for a diff, the `docs/INDEX.md` rule and `no-diff-expected`: [gate rules, Gates](docs/agents/gate-rules.md#gates). Light-delivery tags: [CONFIG.md](docs/CONFIG.md#light-delivery-skip-review-and-skip-validation).
- Cargo gates sharing a target directory run sequentially; overlapping ones need separate `CARGO_TARGET_DIR`s, or a rebuilt `target/debug/orbit` breaks generation-bound fixtures (F2026-10-118).

## Code

- Layering and scoping: [`ARCHITECTURE.md`](ARCHITECTURE.md). Reusable patterns: [`docs/design-patterns/`](docs/design-patterns/).
- Lints live in `[workspace.lints]`: `unwrap`/`expect` warn in production code; propagate errors, with `OrbitError` at crate boundaries. No `print!` (use `tracing`), no lock guards across `.await`. `missing_docs` is enforced in `orbit-auth`, `orbit-clock`, `orbit-config` and `orbit-web`; other crate roots opt out.
- Default to `pub(crate)`, `.workspace = true` deps, bounded channels and typed `thiserror` variants.
- Prefer the fewest moving parts: delete dead code and stale docs together; keep compatibility only for an external contract or persisted format.
- Past ~800 lines, check whether a file holds more than one responsibility and split along those; don't scatter closely related code.
- Greppable names and the skill-doc sync command: [code rules](docs/agents/code-rules.md).
- Never expose internal task/friction IDs in user-facing output, CLI help (Clap renders `///`) or MCP text.
- User-facing CLI help and errors, MCP descriptions and UI text must stand alone: no Orbit source-tree paths or repo docs, only verified published docs or installed resources. Contributor docs may cite source paths; user-workspace paths like `.orbit/tmp/` are fine.

## Tests

- Never add UI tests: no browser, Playwright or DOM-stub tests of the dashboard, web pages, docs site or desktop UI, not even for a bug fix. Check UI changes through manual QA and screenshots.
- Test admission, text matching, test layout and the CI budget: [test and QA rules, Tests](docs/agents/test-and-qa-rules.md#tests). Manual QA, screenshots and STD-07 UI review: [QA](docs/agents/test-and-qa-rules.md#qa).

## Local Rust CodeQL

When it is owed and how to run it: [gate rules, Local Rust CodeQL](docs/agents/gate-rules.md#local-rust-codeql).

<!-- constellation-standards:begin -->
<!-- Managed by the constellation's operations/scripts/sync-standards.sh; edits inside this block are overwritten. -->
## Constellation standards

This repository adopts these constellation standards, vendored read-only in `docs/standards/` (each a directory: `STD-nn.md` holds the rules, `why.md` and `checks.md` the reasons and gates):

- `STD-01@2` — [docs/standards/STD-01-cli-surface/STD-01.md](docs/standards/STD-01-cli-surface/STD-01.md)
- `STD-02@4` — [docs/standards/STD-02-rust-architecture-and-errors/STD-02.md](docs/standards/STD-02-rust-architecture-and-errors/STD-02.md)
- `STD-03@2` — [docs/standards/STD-03-concurrency-and-process-safety/STD-03.md](docs/standards/STD-03-concurrency-and-process-safety/STD-03.md)
- `STD-04@2` — [docs/standards/STD-04-testing-and-verification/STD-04.md](docs/standards/STD-04-testing-and-verification/STD-04.md)
- `STD-05@1` — [docs/standards/STD-05-security-boundaries/STD-05.md](docs/standards/STD-05-security-boundaries/STD-05.md)
- `STD-06@1` — [docs/standards/STD-06-design-docs-and-feature-tracking/STD-06.md](docs/standards/STD-06-design-docs-and-feature-tracking/STD-06.md)
- `STD-07@16` — [docs/standards/STD-07-ui-ux/STD-07.md](docs/standards/STD-07-ui-ux/STD-07.md)
- `STD-08@2` — [docs/standards/STD-08-economy/STD-08.md](docs/standards/STD-08-economy/STD-08.md)

Follow them; they are normative. To deviate from a rule, record a decision in `docs/design/<feature>/4_decisions.md` citing `STD-nn@<version> §Rn`; never edit `docs/standards/` (`sh docs/standards/check.sh` enforces this).
Reviewers check every change against the adopted standards and report violations as `STD-nn §Rn` with file:line evidence.
<!-- constellation-standards:end -->
