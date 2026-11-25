---
type: context
summary: Architecture map for orbit-store: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
tags: [architecture, orbit-store]
---

# orbit-store

## What it owns

Persistence. Record shapes and legacy `*Backend` contracts in `contracts`, the `store` read/write facade (`Store::read`, `Store::write`), private `driver/file` + `driver/sqlite`, concrete record repositories and cross-driver invariants in `repository`, `workflow` import/export/repair, and `compose` construction.

## Main modules and types

`store` (`Store::read` returning `Snapshot`, `Store::write` passing `&Tx`), `contracts` (record shapes and query filters, plus legacy `*Backend` traits such as `TaskStoreBackend` in `crates/orbit-store/src/contracts/traits/task.rs`), `driver`, `repository` (concrete record repositories such as `crates/orbit-store/src/repository/notice.rs` alongside cross-driver repositories), `workflow`, `compose`, `fs`. See ``crates/orbit-store/src/lib.rs``.

## Read and write path

Converted records are reached through two methods on `Store` (`crates/orbit-store/src/store.rs`):

- `Store::read() -> Result<Snapshot<'_>, OrbitError>`: opens one consistent `BEGIN DEFERRED` read view on the pooled `query_only=ON` reader connection (or the in-memory writer connection) and rolls it back on drop. A repository obtained from `snapshot.<record>()` exposes read methods only.
- `Store::write(|tx| …)`: runs a callback inside one `IMMEDIATE` write transaction over the serialized writer connection, committing on `Ok` and rolling back on error or unwind. Only `Store::write` constructs `Tx`, and repository writing methods are exposed only on `tx.<record>()`, so a write outside `Store::write` does not compile.

Each converted record owns one concrete repository module under `crates/orbit-store/src/repository/` (`admission_load.rs`, `application_consent.rs`, `backlog_forecast.rs`, `codebase_fact.rs`, `crew_memory.rs`, `evolution.rs`, `experiment.rs`, `landed_diff.rs`, `notice.rs`, `plugin.rs`, `provider_limit.rs`, `reliability.rs`, `resource_usage.rs`, `retention.rs`, `script_grant.rs`, `tool.rs`), reached only via `snapshot.<record>()` and `tx.<record>()`. Repository methods are named for the record (`get`, `list`, `put`, `close`) and hold SQL and row mapping only; domain rules run in the calling operation inside `Store::write`. Legacy `*Backend` traits in `contracts/` and `driver/sqlite/*_store.rs` coexist on `Store` until their records are converted. A retention mechanic that must not pin one read stays a named second path: `visit_blob_reference_text` takes the `Store` and opens one snapshot per chunk.

The file-backed records (`executor_def.rs`, `policy_def.rs`, `session_log.rs`, `skill.rs`) take the same shape over their directories (`crates/orbit-store/src/file_store.rs`). Each family has a `FileStore<L>` handle, where `L` is its directory layout from `driver/file/` (`ExecutorDefDir`, `PolicyDefDirs`, `SessionLogDir`, `SkillDirs`). `FileStore::read()` returns a `FileSnapshot` and `FileStore::write(|tx| …)` passes a `FileTx`. Records are reached as `snapshot.<record>()` and `tx.<record>()`, and writing methods exist only on `tx.<record>()`. A file write scope is not atomic across files. Each writing method keeps the guarantee it had before: an atomic replace of the one file it writes, and for the session log, the advisory lock held for the whole read-modify-write. The layout keeps paths, locks and durable writes. The repository keeps the YAML, JSON Lines or `SKILL.md` format and the layered workspace-over-global read.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-types`.
- Depended on by: `orbit-automation`, `orbit-cmd`, `orbit-core`, `orbit-engine`, `orbit-runtime`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at [`crates/orbit-store/src/lib.rs`](../../crates/orbit-store/src/lib.rs). The rules for how the store's modules depend on each other (private drivers, `repository` invariants, explicit `workflow` operations, `compose` construction) are in [Crates in ARCHITECTURE.md](../../ARCHITECTURE.md#crates). Task writes commit through one boundary: [task commit boundary](../design-patterns/task_commit_boundary.md).

Design intent: [docs/design/state-compatibility/](../../docs/design/state-compatibility/) — this page describes the code as it is; the design folder holds intent.

## Ownership by module

- `compose`: Composition constructors that assemble driver, repository, and workflow store backends; imports `contracts`, `driver`, `repository`, `workflow`, and `orbit-common`.
- `contracts`: Consumer-facing persistence traits, parameter types, and pure store projections; imports `orbit-common` and `orbit-types` only (no persistence drivers).
- `driver`: Private single-technology persistence drivers (`driver/file` and `driver/sqlite`, never importing each other); imports `contracts`, `fs`, `json_schema`, `orbit-common`, and `orbit-types`.
- `file_store`: Facade exposing `FileStore::read` (`FileSnapshot`) and `FileStore::write` (`FileTx`) over one family of file-backed records and handing out their concrete repositories; imports `driver`, `repository`, and `orbit-common`.
- `fs`: Narrow filesystem mechanics (locks, path safety, atomic writes, YAML) shared by drivers and workflows; imports `orbit-common` and `orbit-types` only.
- `json_schema`: JSON Schema draft validation and compilation helper for stored skill/tool schemas; imports `jsonschema`, `orbit-common`, and `serde_json` only.
- `repository`: Concrete per-record repositories (such as `repository/notice.rs`, reached through `Snapshot` and `Tx`) and cross-driver repositories enforcing invariants across file and SQLite drivers; imports `contracts`, `driver`, `fs`, `store`, `workflow`, `orbit-common`, and `orbit-types`.
- `store`: Facade exposing `Store::read` (`Snapshot`) and `Store::write` (`Tx`) over the host SQLite driver and handing out concrete repositories; imports `driver`, `repository`, and `orbit-common`.
- `workflow`: Explicit one-shot import, export, repair, and layout-upgrade workflows; imports `contracts`, `driver`, `fs`, `repository`, `orbit-common`, and `orbit-types`.
