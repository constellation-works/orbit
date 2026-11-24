---
type: context
summary: Architecture map for orbit-store: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-24
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
