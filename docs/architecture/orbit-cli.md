---
type: context
summary: Architecture map for orbit-cli: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-cli]
---

# orbit-cli

## What it owns

Clap entry point (`orbit` binary) and client configuration. Assembles MCP, Registry, Web, Core; accepting machine resolves its own state and dispatches through Core.

## Main modules and types

`main.rs` (`fn main`), `command`, `entry`, `output`, `parse`, `help`, `plugin_cli`, `audit_middleware`. See ``crates/orbit-cli/src/main.rs``.

`command/tool/run.rs` owns `ToolRunShape`, the prepared operation name and
output policy shared by local execution, host routing and workspace-less notice
aggregation. `ToolRunArgs` requests write sidecars before workspace binding,
then builds the shape from prepared input and applies the compact list hint to
the execution input. Routed calls alone carry the declared model to the owner.

## Dependencies

- Depends on (internal, production): `orbit-cmd`, `orbit-common`, `orbit-config`, `orbit-core`, `orbit-mcp`, `orbit-registry`, `orbit-types`, `orbit-web`.
- Depended on by: — (binary).

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-cli/src/main.rs`.

Design intent: [docs/design/terminal-interface/](../../docs/design/terminal-interface/) — this page describes the code as it is; the design folder holds intent.
