---
type: context
summary: Architecture map for orbit-mcp: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
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

## Ownership by module

- `adapter`: MCP tool dispatch (`OrbitToolServer`), name translation, structured results, and input schemas; imports `error`, `internal_drain`, `presentation_definition`, `orbit-common`, and `orbit-types`.
- `error`: Translation of `OrbitError` values into structured MCP tool error payloads; imports `orbit-common`, `rmcp`, and `serde_json` only.
- `federated`: Fail-closed federated MCP mux routing `hm_*`/`ws_*` selectors across local and SSH-registered hosts; imports `internal_drain`, `remote`, `orbit-common`, `orbit-registry`, `orbit-tools`, and `orbit-types`.
- `internal_drain`: Deterministic owner/follower drain RPC protocol enabled only on the server launch path; imports `orbit-common` only.
- `listener`: TCP listener transport (`McpListener`) for serving the MCP kernel over sockets; imports `orbit-common`, `orbit-types`, `rmcp`, and `tokio` only.
- `listener_peer`: Local user-ID lookup for accepted TCP peers on the MCP listener; imports `listener` and `orbit-common`.
- `presentation_definition`: Adapter-owned read-only presentation tool schema and actions; imports `rmcp` and `serde_json` only.
- `remote`: Thin SSH stdio proxy transport and remote-machine MCP metadata; imports `orbit-common`, `orbit-registry`, `orbit-tools`, and `orbit-types`.
- `stdio_session`: Upgrade-resilient stdio MCP session driver over `OrbitToolServer`; imports `adapter` and `orbit-common`.
- `toolset`: `orbit mcp serve` toolset and role filtering over any `McpHost`; imports `presentation_definition`, `orbit-common`, and `orbit-types`.
