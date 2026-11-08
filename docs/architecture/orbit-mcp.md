---
type: context
summary: Architecture map for orbit-mcp: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-mcp]
---

# orbit-mcp

## What it owns

MCP transport over rmcp: stdio + TCP, tool discovery, per-call traces, SSH stdio proxy, federated mux routing `hm_*`/`ws_*` selectors (fail-closed). Core owns validation/auditing.

## Main modules and types

`adapter` (`OrbitToolServer`, `McpHost` trait in `crates/orbit-mcp/src/lib.rs`), `federated`, `remote`, `listener` (`McpListener`), `toolset`, `stdio_session`, `presentation_definition`. See ``crates/orbit-mcp/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-registry`, `orbit-tools`, `orbit-types`.
- Depended on by: `orbit-cli`, `orbit-cmd`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-mcp/src/lib.rs`.

Design intent: [docs/design/mcp-bridge/](../../docs/design/mcp-bridge/) — this page describes the code as it is; the design folder holds intent.
