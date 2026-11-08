---
type: context
summary: Architecture map for orbit-types: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-types]
---

# orbit-types

## What it owns

Shared serde contracts, pure constructors and narrow domain errors. No I/O.

## Main modules and types

`identity` (`OrbitId`), `task` (`Task`), `workflow`, `policy`, `tool`, `workspace`, `record`, `resource`, `plugin`, `telemetry`, `desktop`. See ``crates/orbit-types/src/lib.rs``.

## Dependencies

- Depends on (internal, production): —.
- Depended on by: all other crates.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-types/src/lib.rs`; start at `crates/orbit-types/src/task/model/task.rs` (`Task`).

Design intent: [docs/design/orbit-core/](../../docs/design/orbit-core/) — this page describes the code as it is; the design folder holds intent.
