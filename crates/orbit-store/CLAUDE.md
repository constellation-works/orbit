# orbit-store

All durable Orbit state that is not a search index. Depends only on `orbit-types` and `orbit-common`; knows nothing of runtimes, commands, or transports. Policy and authorization are `orbit-core`'s; the vector schema is `orbit-search`'s.

- Internal direction is enforced by `scripts/check-dependency-direction.sh` (`make ci-fast`): `contracts` (traits/params/projections; no impls, no `rusqlite`) ← `fs` (locks, path safety, atomic writes, YAML) ← `driver/file` | `driver/sqlite` (never import each other) ← `repository` (live invariants that join drivers) / `workflow` (one-shot import/export/reindex/repair/upgrade) ← `compose` (construction). Retired paths (`src/backend`, `src/file`, `src/sqlite`, `src/state_io`, `src/task_migration`) must not reappear.
- A live write spanning both drivers joins in `repository/`, never in a driver. A one-shot data movement is a `workflow`, never a side effect of opening a store.
- New capability → trait in `contracts`, one impl in one driver, constructor in `compose`. Shared mechanics → `fs`. Concrete construction and migration access stay in composition/bootstrap/`maintenance`.
- SQLite migrations are append-only: never renumber or edit a shipped entry in `driver/sqlite/migration/ledger.rs`. Feature crates use namespaced registries via `migration/feature.rs`, not the global ledger.
- No crate-root `tests/`; the direction guardrail excludes `**/tests/**`, so fixtures may cross layers.
