# Orbit Internal Development Standards

Orbit does not accept external pull requests. Development follows a
single-writer pipeline that executes on a trusted host; running submitted code
from an untrusted branch there would expose the maintainer's credentials.
Unsolicited pull requests will be closed.

GitHub issues remain open for bug reports and feature requests. Outside input
reaches Orbit through issues; implementation is handled by the maintainer and
Orbit's own agents.

## Principles

- Prefer simple, coherent designs over preserving accidental complexity.
- Fix root causes when practical, not just symptoms.
- Keep command, engine, executor, store, and type boundaries clean.
- Treat agent and human experience as product concerns, not just implementation details.

## Setup

```bash
cargo test --workspace
```

Use targeted tests while iterating, then run the full workspace suite before
landing an internal change.

## Toolchain (MSRV)

Orbit's minimum supported Rust version is declared as `rust-version` in the
workspace `Cargo.toml` (`[workspace.package]`) and enforced by the `msrv` job
in `.github/workflows/ci.yml` (`cargo check --workspace --locked` on the
pinned toolchain). If a change genuinely needs a newer compiler or a
dependency bump raises the floor, bump `rust-version` and the workflow's
`MSRV` env var together in the same PR, and call it out in the CHANGELOG.

## Testing & Coverage

CI collects workspace test coverage with
[`cargo-llvm-cov`](https://github.com/taiki-e/cargo-llvm-cov) on every PR
(the `Coverage (informational)` job in `.github/workflows/ci.yml`) and
uploads an lcov report as the `coverage-lcov` workflow artifact. The job is
**informational only — it never gates a merge**.

Per-crate line-coverage **targets** — goals to steer test investment, not
gates that fail CI:

| Crate | Target | Why |
|---|---|---|
| `orbit-policy` | > 90% | Policy evaluation is the security decision surface; on Linux it is the only enforcement layer. |
| `orbit-core` | > 80% | Composition root and command handling — regressions here surface everywhere. |
| `orbit-exec` | > 70% | Process spawning/sandboxing is platform-conditional, so some paths are unreachable on any single CI runner. |

When touching those crates, check the coverage summary in the CI job log (or
run `cargo llvm-cov -p <crate> --summary-only` locally) and prefer adding
tests that close the gap toward the target.

## Repository Shape

Rust workspace crates live under `crates/` (for example `crates/orbit-cli`).

- `orbit-cli`: CLI entrypoint
- `orbit-core`: composition root, command handling, runtime wiring
- `orbit-engine`: job and activity execution engine
- `orbit-tools`, `orbit-agent`, `orbit-store`, `orbit-types`, `orbit-policy`, `orbit-exec`: supporting runtime layers

## Change Expectations

- Keep changes scoped and intentional.
- Add or update tests when behavior changes.
- Prefer removing legacy paths over carrying compatibility code when the product is still pre-adoption.
- If you discover friction or recurring issues, fix them in scope or create a concrete follow-up task.

## Supply-chain (cargo-deny)

Dependencies are gated by [`cargo-deny`](https://embarkstudios.github.io/cargo-deny/)
on every PR (via `scripts/ci-guardrails.sh`) and locally with `make audit`. The
policy lives in [`deny.toml`](deny.toml): it denies crates with an open RUSTSEC
advisory or a yanked version, and restricts licenses to a reviewed allow-list.

Run it before landing an internal dependency change:

```bash
cargo install cargo-deny --locked   # one-time
make audit                          # == cargo deny check
```

**Adding a license.** If a new dependency introduces a license not in the
`[licenses].allow` list, `cargo deny check` fails. Add the SPDX identifier to
the list in `deny.toml` **only** if it is a permissive/public-domain-equivalent
license, with a one-line comment naming the crate(s) and (for weak-copyleft
licenses such as MPL-2.0) a short justification. Copyleft licenses that would
impose obligations on Orbit's own sources must not be added — replace the
dependency instead.

**Advisory exceptions.** Only when there is no safe upgrade available may an
advisory be time-boxed in `[advisories].ignore`. Each entry must be an object
carrying:

- `id` — the `RUSTSEC-YYYY-NNNN` identifier, and
- `reason` — why it is safe in Orbit's usage (why the vulnerable path is
  unreachable or the impact is bounded) **and** a `Re-review YYYY-MM-DD` date
  (default: ~6 months out).

Re-review ignored advisories on or before their date and drop the entry once an
upstream fix lands. Never ignore an advisory that has an available patched
release — bump the dependency instead.

**Isolated advisory database override.** In managed runners or sandboxed validation
where `~/.cargo/advisory-dbs` is read-only (such as containerized workers), configure
an explicit writable advisory database path via `CARGO_DENY_DB_PATH` (or
`ORBIT_CARGO_DENY_DB_PATH`). The canonical wrapper (`scripts/cargo-deny.sh`)
synthesizes an isolated configuration pointing `[advisories].db-path` to that
directory so cargo-deny can acquire its database lock without writing to the home
directory. Cleanup of the temporary scratch directory or fixture is owned by the
invoking runner.

**Offline validation & snapshot provenance.** To validate offline without network
fetching, set `CARGO_DENY_DISABLE_FETCH=1` (or `ORBIT_CARGO_DENY_DISABLE_FETCH=1` /
`CARGO_DENY_OFFLINE=1`). The advisory database snapshot is expected to be cloned
from upstream [`https://github.com/rustsec/advisory-db`](https://github.com/rustsec/advisory-db)
(with default target directory `advisory-db-3157b0e258782691`) or provided by the
environment fixture. Missing, stale, or unrefreshable required data yields an explicit
failure, never a success-by-skip.


## Orbit State

Orbit keeps operational state under `.orbit/`. Review those changes carefully before committing.

- Do not accidentally commit noisy runtime artifacts.
- Treat tracked asset changes as product changes.
- Treat mutable run/task state as operational data unless the change is intentional.

## Commits

- Use clear commit messages.
- Agent-authored commits should use the agent commit identity for that commit.
- Do not leave the repository configured with the agent identity afterward.
