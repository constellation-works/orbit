---
type: context
summary: Architecture map for orbit-store: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
tags: [architecture, orbit-store]
---

# orbit-store

## What it owns

Persistence. Record shapes and query parameters in `contract` (no backend traits, no application rules), the `store` and `file_store` read/write facades (`Store::read`, `Store::write`, `FileStore::read`, `FileStore::write`), private `driver/file` + `driver/sqlite`, concrete per-record and cross-driver repositories in `repository`, task commit and recovery in `boundary`, one-shot import/export/repair/layout-upgrade in `migrate`, and `compose` construction.

## Main modules and types

`store` (`Store::read` returning `Snapshot`, `Store::write` passing `&Tx`), `file_store` (`FileStore::read` returning `FileSnapshot`, `FileStore::write` passing `&FileTx`, plus `AutoTaskStore`), `contract` (record shapes and query parameters), `driver` (private SQLite and file drivers with private writer connections), `repository` (concrete per-record repositories such as `crates/orbit-store/src/repository/notice.rs` alongside cross-driver repositories), `boundary` (task commits and recovery), `migrate` (import, export, repair, layout upgrade), `compose`, `fs`, `json_schema`. See `crates/orbit-store/src/lib.rs`.

## Read and write path

Every SQLite record on `orbit.db` is reached through two methods on `Store` (`crates/orbit-store/src/store.rs`):

- `Store::read() -> Result<Snapshot<'_>, OrbitError>`: opens one consistent `BEGIN DEFERRED` read view on the pooled `query_only=ON` reader connection (or the in-memory writer connection) and rolls it back on drop. A repository obtained from `snapshot.<record>()` exposes read methods only.
- `Store::write(|tx| …)`: runs a callback inside one `IMMEDIATE` write transaction over the serialized writer connection, committing on `Ok` and rolling back on error or unwind. Only `Store::write` constructs `Tx`, the writer connection is private to `crates/orbit-store/src/driver/sqlite/host/connection.rs`, and repository writing methods are exposed only on `tx.<record>()`, so a production write outside `Store::write` does not compile.

Each record owns one concrete repository module under `crates/orbit-store/src/repository/` (`admission_load.rs`, `application_consent.rs`, `crates/orbit-store/src/repository/audit/mod.rs`, `crates/orbit-store/src/repository/automation/mod.rs`, `backlog_forecast.rs`, `codebase_fact.rs`, `crates/orbit-store/src/repository/condition.rs`, `crew_memory.rs`, `drain_intent.rs`, `evolution.rs`, `experiment.rs`, `crates/orbit-store/src/repository/friction/mod.rs`, `incident.rs`, `crates/orbit-store/src/repository/invocation/mod.rs`, `crates/orbit-store/src/repository/job_run/mod.rs`, `landed_diff.rs`, `lease_notice.rs`, `local_pull.rs`, `notice.rs`, `plugin.rs`, `provider_limit.rs`, `reliability.rs`, `resource_usage.rs`, `retention.rs`, `crates/orbit-store/src/repository/review/mod.rs`, `routine.rs`, `script_grant.rs`, `tool.rs`), reached only via `snapshot.<record>()` and `tx.<record>()`. Repository methods are named for the record (`get`, `list`, `put`, `close`) and hold SQL and row mapping only; domain rules run in the calling operation inside `Store::write`. No `*Backend` traits remain in `orbit-store`. Two named second paths stay documented beside `Store::read`/`Store::write`: `Store::append_audit_event_at_path` for read-only/foreign-generation audit lines that must survive outside a writable handle, and `visit_blob_reference_text` for bounded retention chunks that must not pin one read snapshot.

The host recovery authority (`authority.db`) is a separate SQLite file under `crates/orbit-store/src/driver/sqlite/authority/` with its concrete repository in `crates/orbit-store/src/repository/recovery_authority.rs`.

The task index (the `index.sqlite` file under the global root's `tasks` directory) is a separate file with the same shape. `crates/orbit-store/src/driver/sqlite/task_index/` holds its file name, the one place it is opened, its pragmas, its private writer connection, and its schema, whose `crates/orbit-store/src/driver/sqlite/task_index/schema/mod.rs` is the one ordered step list. Its SQL and row mapping are `TaskRegistryStore` in `crates/orbit-store/src/repository/task_index/`, which reaches the file only through the driver handle: `read` for a pooled reader, `write` for one IMMEDIATE transaction, and `cache_write` as the named second path for derived caches that must never wait for the write lock. Task callers hold the concrete `TaskV2Store`.

Workspace composition also supplies `TaskCoordination` for owner-wave admission, execution-claim resolution, and worker-claim verification. These operations use the workspace's shared `TaskCommitBoundary`. With legacy uncoordinated composition, owner-wave admission admits every candidate without publishing its footprint, and the two claim operations report that the claim lifecycle is unavailable.

Review admission, settlement and reconciliation apply their rules in Core while holding one write transaction. Automation checkpoint transitions and condition-trigger fire validation run in `orbit-automation` under the same transaction as their state and receipt or audit writes. Incident reports read bounded failure evidence through `Snapshot::incident`. Job-run admission, lifecycle and checkpoint-state transitions run in `orbit-runtime`'s `JobRuns` inside one write transaction. Read-only pull occupancy queries use `Store::read` snapshots. `allocate_pull_request` computes occupancy inside its `Store::write` transaction before reserving capacity. A landed lease notice is written through `Tx::lease_notice`; overlap matching stays in the task-reservation repository, whose transactions record overlaps too. Core groups those events after the snapshot closes, using the same failure classifier as SQLite's audit aggregates.

The file-backed records (`executor_def.rs`, `policy_def.rs`, `session_log.rs`, `skill.rs`) take the same shape over their directories (`crates/orbit-store/src/file_store.rs`). Each family has a `FileStore<L>` handle, where `L` is its directory layout from `driver/file/` (`ExecutorDefDir`, `PolicyDefDirs`, `SessionLogDir`, `SkillDirs`). `FileStore::read()` returns a `FileSnapshot` and `FileStore::write(|tx| …)` passes a `FileTx`. Records are reached as `snapshot.<record>()` and `tx.<record>()`, and writing methods exist only on `tx.<record>()`. A file write scope is not atomic across files, and its writing methods do not share one per-file guarantee. Each YAML `put` (`executor_def`, `policy_def`) atomically replaces the one file it writes. `executor_def` `delete` unlinks its file and writes no replacement. Session-log `resolve` atomically replaces the whole log. Session-log `append` does not replace the file: it adds one row in place under the advisory lock and syncs the data, so a crash mid-append can leave a torn, unterminated final row. Session-log reads and writes hold the advisory lock for their whole operation; reads ignore that torn tail and the next append repairs it. The layout keeps paths, locks and durable writes. The repository keeps the YAML, JSON Lines or `SKILL.md` format and the layered workspace-over-global read.

Auto-task cursors use `AutoTaskStore::read` and `AutoTaskStore::write` in the same facade. The write scope holds the stable cursor sidecar lock across the scheduler's read, mint, and explicit checkpoints. Each cursor replacement commits on its own so mint recovery can read the last durable checkpoint after a later failure. The friction import reads the legacy Markdown codec through a file snapshot and writes the live rows and the import marker in one SQLite transaction. The friction tag taxonomy remains workspace YAML configuration with lazy default seeding.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-types` (dev: `orbit-store` for `test-support`).
- Depended on by: `orbit-automation`, `orbit-cmd`, `orbit-core`, `orbit-engine`, `orbit-runtime`.
- `rusqlite` in production (`[dependencies]`) is confined to `orbit-store`, `orbit-search`, `orbit-auth`, and `orbit-common`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at [`crates/orbit-store/src/lib.rs`](../../crates/orbit-store/src/lib.rs). The rules for how the store's modules depend on each other (private drivers, `repository` invariants, explicit `migrate` operations, `compose` construction) are in [Crates in ARCHITECTURE.md](../../ARCHITECTURE.md#crates). Task writes commit through one boundary: [task commit boundary](../design-patterns/task_commit_boundary.md).

Design intent: [docs/design/state-compatibility/](../../docs/design/state-compatibility/) — this page describes the code as it is; the design folder holds intent.

## Ownership by module

- `boundary`: Coordinates task mutations that span canonical files, the registry index and host reservations, including journal recovery and bulk maintenance writes. The document, history, artifact and reservation repositories share the workspace’s commit boundary.
- `compose`: Composition constructors that assemble driver, repository, and migration store handles; imports `contract`, `driver`, `repository`, `migrate`, and `orbit-common`.
- `contract`: Record shapes and query parameters with no backend traits or application rules; imports `orbit-common` and `orbit-types` only (no persistence drivers, no `rusqlite`).
- `driver`: Private single-technology persistence drivers (`driver/file` and `driver/sqlite`, never importing each other) with private SQLite writer connections; imports `contract`, `fs`, `json_schema`, `orbit-common`, and `orbit-types`. The host store, task registry, and recovery authority own separate ordered registries run by `orbit-common`'s SQLite ledger.
- `file_store`: Facade exposing `FileStore::read` (`FileSnapshot`) and `FileStore::write` (`FileTx`) over one family of file-backed records and handing out their concrete repositories; imports `driver`, `repository`, and `orbit-common`.
- `fs`: Narrow filesystem mechanics (locks, path safety, atomic writes, YAML) shared by drivers and migrations; imports `orbit-common` and `orbit-types` only.
- `json_schema`: JSON Schema draft validation and compilation helper for stored skill/tool schemas; imports `jsonschema`, `orbit-common`, and `serde_json` only.
- `migrate`: Explicit one-shot import, export, repair, and layout-upgrade migrations, plus the friction submission shared with the task commit boundary; imports `contract`, `driver`, `fs`, `repository`, `orbit-common`, and `orbit-types`.
- `repository`: Concrete per-record repositories (such as `repository/notice.rs`, reached through `Snapshot` and `Tx`) and cross-driver repositories enforcing invariants across file and SQLite drivers; imports `contract`, `driver`, `fs`, `store`, `migrate`, `orbit-common`, and `orbit-types`.
- `store`: Facade exposing `Store::read` (`Snapshot`) and `Store::write` (`Tx`) over the host SQLite driver and handing out concrete repositories; imports `driver`, `repository`, and `orbit-common`.
