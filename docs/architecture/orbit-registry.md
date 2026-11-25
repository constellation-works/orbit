---
type: context
summary: Architecture map for orbit-registry: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
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

## Ownership by module

- `hosts`: Operator-registered remote hosts (`~/.orbit/hosts.toml`) persistence and lookup; imports `machine_identity`, `orbit-common`, and `orbit-types`.
- `machine_identity`: Machine identity (`[machine]` in global `config.toml`) admission, generation, and inspection; imports `orbit-common`, `orbit-config`, and `orbit-types`.
- `workspace_registry`: Logical workspace catalog, checkout bindings, and machine-local registry persistence; imports `orbit-common` and `orbit-types` only.
