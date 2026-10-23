# Contributing to Orbit

Thanks for pitching in. Orbit welcomes bug fixes, docs, new features, new agent executors, plugins, and skills.

## Ways to help

- **Report a bug or request a feature.** Use the [issue templates](https://github.com/constellation-works/orbit/issues/new/choose). Report security issues privately instead, following [SECURITY.md](SECURITY.md).
- **Fix something small.** Typos, docs, error messages, and focused bug fixes can go straight to a pull request.
- **Build something bigger.** Open an issue first for new features, new commands or tools, or changes to persisted formats, so we can agree on the approach before you write it.
- **Improve the docs.** The website lives in [`website/`](website/) and the design docs in [`docs/design/`](docs/design/).

Areas where help goes furthest: locking, worktree management, execution primitives, reconciliation, audit coverage, tool interfaces, and support for more agent CLIs.

## Set up

You need Rust 1.89 or newer, `git`, and `rg` (ripgrep). Running the `orbit-cli`
tests also needs `uv` on `PATH` (CI uses 0.11.28). To run the website, you also
need Node 22.12 or newer.

```bash
git clone https://github.com/constellation-works/orbit && cd orbit
git switch agent-main
make build          # or: make install, which puts orbit in ~/.orbit/bin
cargo test -p <crate>
```

Start with [ARCHITECTURE.md](ARCHITECTURE.md) for the crate layout and layering rules.

## Make a change

1. **Branch from `agent-main`,** and target your PR at `agent-main`. `main` is reserved for releases.
2. **Keep it scoped.** One change per PR, with no unrelated refactors.
3. **Follow the code rules** in [CLAUDE.md](CLAUDE.md#code). Lints are enforced, behaviour is tested at the boundary first (integration, golden, e2e) with unit tests only by exception ([test_strategy.md](docs/design-patterns/test_strategy.md)) in a sibling `tests/` dir, and internal IDs stay out of user-facing text.
4. **Update docs in the same PR.** That includes the affected pages in `docs/design/` and `website/`, and `make goldens UPDATE=1` if you changed CLI help or the MCP surface. Leave `CHANGELOG.md` alone, because it's compiled at release.
5. **Run focused local checks** before opening the PR. For a diff, run the
   checks relevant to the change, such as tests for the behavior you changed,
   `make goldens` when you change CLI help, MCP text or another golden surface,
   and Clippy for Rust crates you touched. The required PR checks —
   `Check / Clippy / Test` and `Test shard 1/3`, `2/3` and `3/3` — cover
   formatting, guardrails, lint, rustdoc, goldens and the full test suite. For
   managed Orbit deliveries, the pipeline runs the commands in
   `task.final_validation_commands`, configured through
   `workflow.required_validation_commands`. See
   [Validation and CI](docs/DEVELOPMENT.md#validation-and-ci) for details.

6. **Write a clear commit message** with a type prefix: `feat:`, `fix:`, `docs:`, `refactor:`, or `chore:`.

Tests that mutate Orbit state, spawn pipeline runs, or depend on the sandbox have specific isolation rules. Read [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) before writing one. The same file covers coverage targets, MSRV bumps, dependency and license policy, and vendored dashboard JS.

## Review

- Pull requests require `Check / Clippy / Test` and `Test shard 1/3`, `2/3` and `3/3`. Fork PRs get no secrets.
- Linux CI cancels superseded runs for the same PR. The macOS workflow runs only on pushes to `main` and `agent-main`, never on pull requests. Its push runs share one group per branch, so a branch has at most one running and one waiting run, and a newer push replaces the waiting one. An intermediate `agent-main` commit can therefore get no macOS run; the newest commit always gets one. `main` and `agent-main` queue separately.
- A maintainer reviews every PR on GitHub and squash-merges it into `agent-main`. Maintainers never run contributed branches through Orbit's own pipeline host.
- Expect a first response within a week. If a PR stalls, a comment is welcome.

## Using coding agents

Agent-assisted PRs are welcome. [AGENTS.md](AGENTS.md) and [CLAUDE.md](CLAUDE.md) load automatically in most agent CLIs. You are responsible for the diff, so review it, run focused local checks relevant to the changes, and say in the PR description what you verified. Required PR checks and managed-delivery final validation are described in [Validation and CI](docs/DEVELOPMENT.md#validation-and-ci).

## Community

Everyone taking part agrees to the [Code of Conduct](CODE_OF_CONDUCT.md). Contributions are licensed under the project's [MIT License](LICENSE.md).
