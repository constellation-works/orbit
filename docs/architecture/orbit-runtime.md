---
type: context
summary: Architecture map for orbit-runtime: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-runtime]
---

# orbit-runtime

## What it owns

Runtime kernel: `RuntimeKernel`, stores, engine handles, event bus, audit, claims, reservations, plugin host, workspace binding, host probes, process/tool execution. Never depends on `orbit-core`; finishes use-cases via `ApplicationPort`.

## Main modules and types

`orbit_runtime` (`RuntimeKernel`), `builder`, `engine`, `audit`, `event_bus`, `plugin`, `workspace`, `application_port` (`ApplicationPort`), `context`, `cost`. See ``crates/orbit-runtime/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-config`, `orbit-engine`, `orbit-exec`, `orbit-policy`, `orbit-search`, `orbit-store`, `orbit-tools`, `orbit-types`.
- Depended on by: `orbit-core`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-runtime/src/lib.rs`.

Design intent: [docs/design/orbit-core/](../../docs/design/orbit-core/) — this page describes the code as it is; the design folder holds intent.
