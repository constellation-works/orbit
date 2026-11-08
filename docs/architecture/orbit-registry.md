---
type: context
summary: Architecture map for orbit-registry: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-registry]
---

# orbit-registry

## What it owns

This machine's `[machine]` identity, workspace catalog, checkout bindings, task-publication bindings, registered remote hosts (`hosts.toml`). No shared DB, no runtime execution.

## Main modules and types

`machine_identity` (`MachineIdentity`), `workspace_registry`, `hosts`. See ``crates/orbit-registry/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-config`, `orbit-types`.
- Depended on by: `orbit-cli`, `orbit-cmd`, `orbit-core`, `orbit-mcp`, `orbit-web`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-registry/src/lib.rs`.

Design intent: [docs/design/host-registry/](../../docs/design/host-registry/) — this page describes the code as it is; the design folder holds intent.
