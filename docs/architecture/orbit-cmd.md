---
type: context
summary: Architecture map for orbit-cmd: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-27
tags: [architecture, orbit-cmd]
---

# orbit-cmd

## What it owns

Composes the application for CLI and web: given flags and an optional workspace selection, it opens a `Core` for the selected registered checkout and returns nothing else that holds the runtime. It also holds command groups, routines, managed-worker transports, the doctor and host apps' ports, host routing and update.

## Main modules and types

`doctor_host`, `hosts`, `update`, `mcp_clients`, `log_format`, `registry`, `task`. See ``crates/orbit-cmd/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-config`, `orbit-core`, `orbit-mcp`, `orbit-registry`, `orbit-runtime`, `orbit-store`, `orbit-tools`, `orbit-types`.
- Depended on by: `orbit-cli`, `orbit-web`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-cmd/src/lib.rs`.

Design intent: [docs/design/terminal-interface/](../../docs/design/terminal-interface/) — this page describes the code as it is; the design folder holds intent.

## Ownership by module

- `agent_rules`: Opt-in injection of Orbit workflow rules into workspace `CLAUDE.md` and `AGENTS.md` files; imports `orbit-common` only.
- `doctor_host`: The doctor app's `Host` port: the host registry, MCP client registrations, fleet routine statuses, the browser kit, task-store partitions and managed-service environments that `orbit-core`'s `app::doctor` cannot reach; imports `hosts`, `mcp_clients`, `registry`, `service_environment`, `task`, `orbit-common`, `orbit-core`, and `orbit-types`.
- `hosts`: The host app's `Probe` port over the federated MCP probe and its `PullDrains` port, which opens each replica checkout read-only as a `Core`, and task-id and `--host` routing to registered hosts; imports `registry`, `task`, `orbit-common`, `orbit-core`, `orbit-mcp`, `orbit-registry`, `orbit-store`, and `orbit-types`.
- `log_format`: Process-log filters, path resolution, and plain-text source/code columns shared by `orbit log` and the dashboard; imports `orbit-common` and `orbit-core`.
- `mcp_clients`: Supported MCP client configuration paths and registration checks for `orbit mcp init/remove`; imports `orbit-common` only.
- `migrate`: Versioned `.orbit/` upgrade surface behind `orbit migrate`; imports `orbit-common`, `orbit-config`, `orbit-core`, and `orbit-store`.
- `registry`: Registry-backed workspace opening, which attaches registry facts to an `orbit_core::app::workspace::Opening` and returns a `Core`, and routine command helpers; imports `worker_coordination`, `workspace_catalog`, `workspace_rebind`, and lower workspace crates.
- `service_environment`: Names-only checks of the managed launcher's live environment; imports `orbit-common`, `orbit-core`, and `orbit-types`.
- `task`: Task-store and owner resolution commands; imports `registry`, `orbit-common`, `orbit-core`, `orbit-registry`, `orbit-runtime`, `orbit-store`, and `orbit-types`.
- `test_support`: The registered opens downstream crates' tests seed through, compiled only for tests; imports `registry`, `orbit-common`, `orbit-core`, `orbit-runtime`, and `orbit-types`.
- `update`: Published Orbit release installation and upgrade convergence pipeline (`orbit update`); imports `registry`, `orbit-common`, and `orbit-core`.
- `worker_coordination`: Attaches owner MCP transports to an opening for managed workers and follower pull drains; imports `hosts`, `orbit-common`, `orbit-core`, `orbit-mcp`, `orbit-registry`, `orbit-runtime`, `orbit-tools`, and `orbit-types`.
- `workspace_catalog`: Registry-backed workspace catalog resolution for Core's federated-search scope; imports `doctor_host`, `registry`, `orbit-common`, `orbit-core`, `orbit-registry`, `orbit-runtime`, and `orbit-types`.
- `workspace_rebind`: Host-local checkout relocation with durable registry decision before projections; imports `orbit-common`, `orbit-core`, `orbit-registry`, `orbit-runtime`, `orbit-store`, and `orbit-types`.
- `workspace_removal`: Workspace removal composing the workspace catalog with task-store ownership and checkout cleanup; imports `task`, `orbit-core`, `orbit-registry`, and `orbit-types`.
