---
type: context
summary: Architecture map for orbit-cmd: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
tags: [architecture, orbit-cmd]
---

# orbit-cmd

## What it owns

Composes the application for CLI and web: command groups, runtime assembly, routines, managed-worker transports, `orbit doctor`, `orbit host`, update, browser-kit.

## Main modules and types

`doctor` (`DoctorCommands`), `hosts`, `update`, `browser_kit`, `mcp_clients`, `log_format`, `registry`, `task`. See ``crates/orbit-cmd/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-config`, `orbit-core`, `orbit-mcp`, `orbit-registry`, `orbit-store`, `orbit-tools`, `orbit-types`.
- Depended on by: `orbit-cli`, `orbit-web`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-cmd/src/lib.rs`.

Design intent: [docs/design/terminal-interface/](../../docs/design/terminal-interface/) — this page describes the code as it is; the design folder holds intent.

## Ownership by module

- `agent_rules`: Opt-in injection of Orbit workflow rules into workspace `CLAUDE.md` and `AGENTS.md` files; imports `orbit-common` only.
- `browser_kit`: Installation, reporting, verification, and removal of the host's headless browser kit (`orbit config browser-kit`); imports `orbit-common` and `orbit-core`.
- `doctor`: Whole-workspace self-diagnostics behind `orbit doctor`; imports `browser_kit`, `hosts`, `mcp_clients`, `registry`, `service_environment`, `task`, and lower workspace crates.
- `hosts`: Registered remote host management (`orbit host`) joining registry records to federated MCP probes; imports `registry`, `task`, `orbit-common`, `orbit-core`, `orbit-mcp`, `orbit-registry`, `orbit-store`, and `orbit-types`.
- `log_format`: Process-log filters, path resolution, and plain-text source/code columns shared by `orbit log` and the dashboard; imports `orbit-common` and `orbit-core`.
- `mcp_clients`: Supported MCP client configuration paths and registration checks for `orbit mcp init/remove`; imports `orbit-common` only.
- `migrate`: Versioned `.orbit/` upgrade surface behind `orbit migrate`; imports `orbit-common`, `orbit-config`, `orbit-core`, and `orbit-store`.
- `registry`: Registry-backed runtime opening and routine command helpers; imports `worker_coordination`, `workspace_catalog`, `workspace_rebind`, and lower workspace crates.
- `service_environment`: Names-only checks of the managed launcher's live environment; imports `orbit-common`, `orbit-core`, and `orbit-types`.
- `task`: Task-store and owner resolution commands; imports `registry`, `orbit-common`, `orbit-core`, `orbit-registry`, `orbit-store`, and `orbit-types`.
- `update`: Published Orbit release installation and upgrade convergence pipeline (`orbit update`); imports `registry`, `orbit-common`, and `orbit-core`.
- `worker_coordination`: Runtime composition with owner MCP transports for managed workers and follower pull drains; imports `hosts`, `orbit-common`, `orbit-core`, `orbit-mcp`, `orbit-registry`, `orbit-tools`, and `orbit-types`.
- `workspace_catalog`: Registry-backed workspace catalog resolution for Core's federated-search scope; imports `doctor`, `registry`, `orbit-common`, `orbit-core`, `orbit-registry`, and `orbit-types`.
- `workspace_rebind`: Host-local checkout relocation with durable registry decision before projections; imports `orbit-common`, `orbit-core`, `orbit-registry`, `orbit-store`, and `orbit-types`.
- `workspace_removal`: Workspace removal composing the workspace catalog with task-store ownership and checkout cleanup; imports `task`, `orbit-core`, `orbit-registry`, and `orbit-types`.
