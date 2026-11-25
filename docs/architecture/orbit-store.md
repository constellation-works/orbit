---
type: context
summary: Architecture map for orbit-store: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
tags: [architecture, orbit-store]
---

# orbit-store

## What it owns

Persistence. Contracts in `contracts`, private `driver/file` + `driver/sqlite`, `repository` invariants, `workflow` import/export/repair, `compose` construction.

## Main modules and types

`contracts` (`TaskStoreBackend` in `crates/orbit-store/src/contracts/traits/task.rs`), `driver`, `repository`, `workflow`, `compose`, `fs`. See ``crates/orbit-store/src/lib.rs``.

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
- `fs`: Narrow filesystem mechanics (locks, path safety, atomic writes, YAML) shared by drivers and workflows; imports `orbit-common` and `orbit-types` only.
- `json_schema`: JSON Schema draft validation and compilation helper for stored skill/tool schemas; imports `orbit-common` only.
- `repository`: Live repositories enforcing cross-driver invariants across file and SQLite drivers; imports `contracts`, `driver`, `fs`, `workflow`, `orbit-common`, and `orbit-types`.
- `workflow`: Explicit one-shot import, export, repair, and layout-upgrade workflows; imports `contracts`, `driver`, `fs`, `repository`, `orbit-common`, and `orbit-types`.
