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

- There is no required local full-suite gate for a diff. The required PR checks cover formatting, lint, goldens and the full suite; a red check is repaired through the PR. Don't run `make ci`, `ci-fast`, `ci-lint`, `goldens` or `ci-test-affected` as blanket pre-handoff gates. Run what the change needs, and `make goldens` when a golden surface changes (`UPDATE=1` regenerates; review the diff).
- Anchor focused `cargo test -p <crate> …` commands with `-p orbit-cli` so they resolve one workspace feature set and reuse the cached build.
- Pre-handoff checks for a diff, because final validation's `make ci-fast` fails on them (ORB-16683): `make ci-guards`, `cargo fmt --all -- --check` (test files too) and, for each changed crate, the Clippy passes and `cargo doc`. Commands, the `docs/INDEX.md` rule and the light-delivery tags: [Validation and CI](docs/DEVELOPMENT.md#validation-and-ci).
- Cargo gates sharing a target directory run sequentially; overlapping ones need separate `CARGO_TARGET_DIR`s, or a rebuilt `target/debug/orbit` breaks generation-bound fixtures (F2026-10-118).

## Code

- Layering and scoping: [`ARCHITECTURE.md`](ARCHITECTURE.md). Reusable patterns: [`docs/design-patterns/`](docs/design-patterns/).
- Lints live in `[workspace.lints]`: `unwrap`/`expect` warn in production code; propagate errors, with `OrbitError` at crate boundaries. No `print!` (use `tracing`), no lock guards across `.await`. `missing_docs` is enforced in `orbit-auth`, `orbit-clock`, `orbit-config` and `orbit-web`; other crate roots opt out.
- Default to `pub(crate)`, `.workspace = true` deps, bounded channels and typed `thiserror` variants.
- Prefer the fewest moving parts: delete dead code and stale docs together; keep compatibility only for an external contract or persisted format.
- Past ~800 lines, check whether a file holds more than one responsibility and split along those; don't scatter closely related code.
- Names must be greppable: an item used outside its module gets a name that says what it does and is near-unique in the repo, and never encodes its file path. Examples, the skill-doc sync command and user-facing text limits: [code rules](docs/agents/code-rules.md).
- Never expose internal task/friction IDs in user-facing output, CLI help (Clap renders `///`) or MCP text.
- User-facing CLI help and errors, MCP descriptions and UI text must stand alone: no Orbit source-tree paths or repo docs, only verified published docs or installed resources.

## Tests

- Never add UI tests: no browser, Playwright or DOM-stub tests of the dashboard, web pages, docs site or desktop UI, not even for a bug fix. Check UI changes through manual QA and screenshots.
- Add tests only for an observed, reproduced defect: a focused regression check at a public CLI/API boundary that fails before the fix and passes after. No private-function tests and no speculative coverage for features, refactors or chores.
- No text-matching tests (`include_str!` + `contains()` over source, assets or UI copy) except a narrow structural safety guard; don't pin config or prompt policy.
- Admitted unit tests live in a sibling `tests/` dir mirroring source filenames ([`test_layout.md`](docs/design-patterns/test_layout.md)); crate-root `tests/` is integration only.
- The full test, manual QA and screenshot rules: [test and QA rules](docs/agents/test-and-qa-rules.md). UI work goes through [STD-07](docs/standards/STD-07-ui-ux/STD-07.md) first; cite a violation as `STD-07 §Rn`.

## Local Rust CodeQL

- Run it locally only when the task repairs a CodeQL finding, is tagged `security` or `security-review`, or edits `.github/codeql/**` or `scripts/codeql-rust-local.sh`. Then follow the [local CodeQL runbook](docs/runbooks/codeql-local.md).
- Run it once per delivery against the final candidate, in the foreground, with a timeout of at least 45 minutes. Only Linux extracts every module; elsewhere the script exits 3 and the check is owed by a Linux run recorded as `codeql` external evidence.

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
- `STD-07@14` — [docs/standards/STD-07-ui-ux/STD-07.md](docs/standards/STD-07-ui-ux/STD-07.md)
- `STD-08@2` — [docs/standards/STD-08-economy/STD-08.md](docs/standards/STD-08-economy/STD-08.md)

Follow them; they are normative. To deviate from a rule, record a decision in `docs/design/<feature>/4_decisions.md` citing `STD-nn@<version> §Rn`; never edit `docs/standards/` (`sh docs/standards/check.sh` enforces this).
Reviewers check every change against the adopted standards and report violations as `STD-nn §Rn` with file:line evidence.
<!-- constellation-standards:end -->
