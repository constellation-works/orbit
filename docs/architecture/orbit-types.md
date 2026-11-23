---
type: context
summary: Architecture map for orbit-types: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-types]
---

# orbit-types

## What it owns

Shared serde contracts, pure constructors and narrow domain errors. It does no I/O of any kind. Its serde shapes are persisted contracts.

## Main modules and types

`identity` (`OrbitId`), `task` (`Task`), `workflow`, `policy`, `tool`, `workspace`, `record`, `resource`, `plugin`, `telemetry`, `desktop`. See ``crates/orbit-types/src/lib.rs``.

## Dependencies

- Depends on (internal, production): —.
- Depended on by (internal, production): `orbit-agent`, `orbit-auth`, `orbit-automation`, `orbit-cli`, `orbit-cmd`, `orbit-common`, `orbit-config`, `orbit-core`, `orbit-engine`, `orbit-exec`, `orbit-mcp`, `orbit-policy`, `orbit-registry`, `orbit-runtime`, `orbit-search`, `orbit-store`, `orbit-tools`, `orbit-web`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-types/src/lib.rs`; start at `crates/orbit-types/src/task/model/task.rs` (`Task`).

Design intent: [docs/design/orbit-core/](../../docs/design/orbit-core/) — this page describes the code as it is; the design folder holds intent.

## Domain modules

The domain modules are `desktop`, `identity`, `plugin`, `policy`, `record`, `resource`, `task`, `telemetry`, `timestamp`, `tool`, `workflow` and `workspace`. `desktop` = guarded desktop task contracts; `plugin` = plugin manifests, pins, and namespace rules; `record` = durable authored artifacts; `telemetry` = measurement of runs. `timestamp` owns the shared legacy UTC wire format for policy and executor records.
