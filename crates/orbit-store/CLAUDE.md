# orbit-store

Project instructions for the persistence crate.

## One job

All durable Orbit state that is not a search index: task bundles, the
coordination task registry, audit and invocation events, job runs, friction,
routines, reservations and workspace claims, session logs, skills, and
definition files. It depends only on `orbit-types` and `orbit-common`, and it
knows nothing about runtimes, commands, or transports.

Not here: domain policy and authorization (`orbit-core`), the semantic vector
schema (`orbit-search::vector`, which owns its own `rusqlite::Connection`), and
anything that needs to *decide* rather than persist.

## Internal direction is enforced, not conventional

This is one crate with a directional internal graph — the arrows in
[`ARCHITECTURE.md`](../../ARCHITECTURE.md) §"orbit-store internal direction" are
checked by
[`scripts/check-dependency-direction.sh`](../../scripts/check-dependency-direction.sh)
on every `make ci-fast`:

| Layer | Owns | May not import |
|---|---|---|
| [`contracts`](src/contracts) | every consumer-visible trait, param, filter, and projection | any implementation, and `rusqlite` at all |
| [`fs`](src/fs) | advisory locking, path safety, atomic writes, YAML | drivers, repositories, workflows |
| [`driver/file`](src/driver/file) | one persistence technology: files | `driver/sqlite`, `Store`/`StoreTx`, repositories, workflows |
| [`driver/sqlite`](src/driver/sqlite) | one persistence technology: SQLite | `driver/file`, repositories, workflows |
| [`repository`](src/repository) | live invariants that *join* drivers | — |
| [`workflow`](src/workflow) | explicit one-shot import/export/reindex/repair/upgrade | — |
| [`compose`](src/compose) | construction; returns contract-facing types | — |

The two drivers never call each other. When a live write spans both — a task
commit is a canonical bundle write plus registry allocation/index rows — the join belongs in
[`repository/task`](src/repository/task), never in a driver.

A one-shot data movement is a `workflow`, not a hidden side effect of opening a
store. `compose::workspace_friction_store` runs the idempotent, transactional
Markdown import *before* opening the live repository, precisely so construction
stays honest.

The guardrail also fails if a retired ownership path (`src/backend`, `src/file`,
`src/sqlite`, `src/state_io`, `src/task_migration`) reappears. Do not recreate
one.

## Where a change goes

- New consumer-visible capability → a trait/param/projection in `contracts`,
  one implementation in exactly one driver, a constructor in `compose`.
- Shared atomic-write / lock / path-safety / YAML mechanics → `fs`, never a
  backend-shaped utility module next to a driver.
- Ordinary application code consumes the contract traits. Concrete construction
  and migration access stay in composition, bootstrap, and maintenance
  adapters; [`maintenance`](src/lib.rs) is deliberately named as operator-only.

## Migrations are append-only

[`driver/sqlite/migration/ledger.rs`](src/driver/sqlite/migration/ledger.rs)
holds a stable ordered `MIGRATIONS` registry. **Never renumber or edit an entry
that has shipped**, including across reverts and history rewrites — append a
new version instead. Each migration runs in one transaction together with its
ledger insert, and a database recorded newer than `SUPPORTED_SCHEMA_VERSION` is
refused rather than downgraded.

Feature crates get namespaced registries via
[`migration/feature.rs`](src/driver/sqlite/migration/feature.rs): the feature
owns its callbacks and calls `Store::apply_feature_migrations` before exposing
its API; its versions are independent of the global schema version. Use that
seam rather than adding a feature's tables to the global ledger.

## Tests

Sibling `tests/` directories throughout
([`test_layout.md`](../../docs/design-patterns/test_layout.md)); the direction
guardrail excludes `**/tests/**`, so a test may legitimately reach across
layers to build a fixture while production code may not. There is no crate-root
`tests/` directory — a change that seems to need one is usually a change that
belongs behind `contracts` and can be tested through `compose`.
