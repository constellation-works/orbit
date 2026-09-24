# Contributing to Orbit

Thanks for pitching in. Orbit welcomes bug fixes, docs, new features, new agent executors, plugins, and skills.

## Ways to help

- **Report a bug or request a feature.** Use the [issue templates](https://github.com/constellation-works/orbit/issues/new/choose). Report security issues privately instead, following [SECURITY.md](SECURITY.md).
- **Fix something small.** Typos, docs, error messages, and focused bug fixes can go straight to a pull request.
- **Build something bigger.** Open an issue first for new features, new commands or tools, or changes to persisted formats, so we can agree on the approach before you write it.
- **Improve the docs.** The website lives in [`website/`](website/) and the design docs in [`docs/design/`](docs/design/).

Areas where help goes furthest: locking, worktree management, execution primitives, reconciliation, audit coverage, tool interfaces, and support for more agent CLIs.

## Set up

You need Rust 1.89 or newer, `git`, and `rg` (ripgrep). To run the website, you also need Node 18+.

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
3. **Follow the code rules** in [CLAUDE.md](CLAUDE.md#code). Lints are enforced, unit tests go in a sibling `tests/` dir, and internal IDs stay out of user-facing text.
4. **Update docs in the same PR.** That includes the affected pages in `docs/design/` and `website/`, and `make goldens UPDATE=1` if you changed CLI help or the MCP surface. Leave `CHANGELOG.md` alone, because it's compiled at release.
5. **Run the gates** before opening the PR:

   ```bash
   make ci-fast    # fmt and repository guardrails
   make ci-lint    # clippy -D warnings
   make goldens    # CLI help and MCP snapshots
   ```

6. **Write a clear commit message** with a type prefix: `feat:`, `fix:`, `docs:`, `refactor:`, or `chore:`.

Tests that mutate Orbit state, spawn pipeline runs, or depend on the sandbox have specific isolation rules. Read [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) before writing one. The same file covers coverage targets, MSRV bumps, dependency and license policy, and vendored dashboard JS.

## Review

- CI runs the full `make ci` on GitHub-hosted runners. Fork PRs get no secrets.
- A maintainer reviews every PR on GitHub and squash-merges it into `agent-main`. Maintainers never run contributed branches through Orbit's own pipeline host.
- Expect a first response within a week. If a PR stalls, a comment is welcome.

## Using coding agents

Agent-assisted PRs are welcome. [AGENTS.md](AGENTS.md) and [CLAUDE.md](CLAUDE.md) load automatically in most agent CLIs. You are responsible for the diff, so read it, run the gates, and say in the PR description what you verified.

## Community

Everyone taking part agrees to the [Code of Conduct](CODE_OF_CONDUCT.md). Contributions are licensed under the project's [MIT License](LICENSE.md).
