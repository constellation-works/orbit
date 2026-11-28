---
type: context
summary: "Orbit agent guide: rules, branching, gates, code/tests, local CodeQL"
last_validated: 2026-11-22
---

# Orbit — agent guide

Orbit is a local-first Rust coding-agent runtime. `CLAUDE.md` imports this file; for Orbit lifecycle work, invoke the `orbit` skill.

## Rules

- Stay in scope. In managed activities, the pipeline owns commits and delivery transitions. Implementation and PR requests don't authorize merges.
- Use `orbit.task.add` for IDs and `orbit.task.update` for edits. Never invent IDs or edit task files.
- Don't add cross-crate deps without updating [`ARCHITECTURE.md`](ARCHITECTURE.md).
- Historical ADRs are not an authority; judge from current code, tests and requirements.
- Don't touch `CHANGELOG.md`; compiled at release time ([`RELEASING.md`](RELEASING.md)).
- For [breaking changes](RELEASING.md#versioning), the implementer tags the task `breaking-change` and adds one execution-summary line `Breaking: <x removed → use y>`. The reviewer confirms both for release tooling to derive the version bump and changelog bullet.
- Update affected docs in the code PR. Stale docs block review.
- Edit `AGENTS.md` only to add, revise or remove an affected rule, command or pointer. Delete or rewrite rules whose subject disappears. Give new features their own docs.
- Keep incidental updates to the shared reference under `docs/config/` out of a task's `context_files`. The implementer updates those pages in the same PR.
- The `doc-duties` auto-task validates root `AGENTS.md`, `CONTRIBUTING.md`, `SECURITY.md`, `ARCHITECTURE.md` and `RELEASING.md`; keep their `last_validated` frontmatter.
- At handoff list commands as passed, failed or not run. Never say "tested" or count unavailable checks as passes.

## Branching

Reserve `main` for release merges and hotfixes. Target every task PR at the dev integration branch, `agent-main`. For promotion and hotfixes see [`RELEASING.md`](RELEASING.md).

## Gates

- Local full-suite checks are optional. Required PR checks (Check/Clippy/Test, Test shards 1–3) cover format, lint, goldens and the full suite. Fix failures in the PR. Don't run `make ci`, `ci-fast`, `ci-lint`, `goldens` or `ci-test-affected` as blanket gates. Run what the change needs: affected tests and `make goldens` when CLI help, MCP text or other golden surfaces change (`UPDATE=1` regenerates; review the diff). See [dev docs](docs/development/validation-and-ci.md#validation-and-ci).
- Anchor focused `cargo test -p <crate> …` on `-p orbit-cli` so they share one workspace feature set and reuse the cached build.
- Pre-handoff checks, the `docs/INDEX.md` rule and `no-diff-expected`: [gate rules, Gates](docs/agents/gate-rules.md#gates). Light-delivery tags: [configuration reference](docs/config/workflow-tags.md#light-delivery-skip-review-and-skip-validation).
- Cargo gates sharing a target directory run sequentially; overlapping ones need separate `CARGO_TARGET_DIR`s; a rebuilt `target/debug/orbit` breaks generation-bound fixtures (F2026-10-118).

## Code

- Layering, scoping: [`ARCHITECTURE.md`](ARCHITECTURE.md). Reusable patterns: [`docs/design-patterns/`](docs/design-patterns/).
- `[workspace.lints]` warns on `unwrap`/`expect`. Propagate `OrbitError` across crates. Use `tracing`. No `print!` or locks across `.await`. `missing_docs` warns in `orbit-auth`, `orbit-cli` (binary), `orbit-clock`, `orbit-common`, `orbit-config`, `orbit-web` and `orbit-desktop`. Other library roots opt out.
- Default to `pub(crate)`, `.workspace = true` deps, bounded channels, typed `thiserror` variants.
- Prefer the fewest moving parts: delete dead code and stale docs together; compatibility only for an external contract or persisted format.
- Past ~800 lines, split a file with more than one responsibility along those; don't scatter closely related code.
- Greppable names and the skill-doc sync command: [code rules](docs/agents/code-rules.md).
- Never expose internal task/friction IDs in user-facing output, CLI help (Clap renders `///`) or MCP text.
- User-facing CLI help/errors, MCP descriptions and UI text must stand alone: no Orbit source-tree paths or repo docs, only verified published docs or installed resources. Contributor docs may cite source paths; user-workspace paths like `.orbit/tmp/` may too.

## Tests

- Never add UI tests: no browser, Playwright or DOM-stub tests of the dashboard, web pages, docs site or desktop UI, not even for a bug fix. Check UI changes with manual QA and screenshots.
- Test admission, text matching, layout and the CI budget: [test and QA rules, Tests](docs/agents/test-and-qa-rules.md#tests). Manual QA, screenshots and STD-07 UI review: [QA](docs/agents/test-and-qa-rules.md#qa).

## Local Rust CodeQL

Required only for CodeQL fixes, tasks tagged `security` or `security-review`, or edits to `.github/codeql/**` or `scripts/codeql-rust-local.sh`. Only Linux extracts every module. Elsewhere the script exits 3. Record the required Linux run as `codeql` external evidence. See [gate rules, Local Rust CodeQL](docs/agents/gate-rules.md#local-rust-codeql).

<!-- constellation-standards:begin -->
<!-- Managed by the constellation's operations/scripts/sync-standards.sh; edits inside this block are overwritten. -->
## Constellation standards

This repository adopts these constellation standards, vendored read-only in `docs/standards/` (each a directory: `STD-nn.md` holds the rules, `why.md` and `checks.md` the reasons and gates):

- `STD-01@4` — [docs/standards/STD-01-cli-surface/STD-01.md](docs/standards/STD-01-cli-surface/STD-01.md): CLI surface — command grammar, inputs and effects, output modes, errors, help, and golden-tested public surfaces
- `STD-02@11` — [docs/standards/STD-02-rust-architecture-and-errors/STD-02.md](docs/standards/STD-02-rust-architecture-and-errors/STD-02.md): Rust architecture and errors — layering, visibility, typed errors, failure semantics, idiom and naming, logging, and…
- `STD-03@2` — [docs/standards/STD-03-concurrency-and-process-safety/STD-03.md](docs/standards/STD-03-concurrency-and-process-safety/STD-03.md): Concurrency and process safety — locks, channels, durable writes, state evolution, subprocess lifecycles, destructive…
- `STD-04@2` — [docs/standards/STD-04-testing-and-verification/STD-04.md](docs/standards/STD-04-testing-and-verification/STD-04.md): Testing and verification — test design, hermetic tests, honest gates, docs as contracts, and evidence-based…
- `STD-05@1` — [docs/standards/STD-05-security-boundaries/STD-05.md](docs/standards/STD-05-security-boundaries/STD-05.md): Security boundaries — authority, filesystem containment, child environments, redaction, local network surfaces, supply…
- `STD-06@4` — [docs/standards/STD-06-design-docs-and-feature-tracking/STD-06.md](docs/standards/STD-06-design-docs-and-feature-tracking/STD-06.md): Design docs and feature tracking — one folder per feature, a delivery stage separate from doc approval, a generated…
- `STD-07@20` — [docs/standards/STD-07-ui-ux/STD-07.md](docs/standards/STD-07-ui-ux/STD-07.md): UI and UX — consistent spacing, shared edges, one type scale, minimal words, show each thing once, honest charts…
- `STD-08@4` — [docs/standards/STD-08-economy/STD-08.md](docs/standards/STD-08-economy/STD-08.md): Economy — bounded agent context, delta-only records, one channel per ask, bookkeeping off the page, one-line errors…
- `STD-12@4` — [docs/standards/STD-12-source-style/STD-12.md](docs/standards/STD-12-source-style/STD-12.md): Source style — names, vertical spacing, line width and the formatter gate, as a language-neutral core with one profile…

Follow them; they are normative. To deviate from a rule, record a decision in `docs/design/<feature>/4_decisions.md` citing `STD-nn@<version> §Rn`; never edit `docs/standards/` (`sh docs/standards/check.sh` enforces this).
Reviewers check every change against the adopted standards and report violations as `STD-nn §Rn` with file:line evidence.
<!-- constellation-standards:end -->
