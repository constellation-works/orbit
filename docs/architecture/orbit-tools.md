---
type: context
summary: Architecture map for orbit-tools: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-tools]
---

# orbit-tools

## What it owns

Tool registry and built-in fs/exec/Orbit tool definitions; plugin loading (`plugin.yaml` v2, exec/mcp backends); sole owner of the `gh` CLI contract.

## Main modules and types

`registry` (`ToolRegistry` in `crates/orbit-tools/src/registry.rs`), `builtin`, `plugin`, `github_cli`, `mcp` (`builtin_mcp_rules`), `external`, `tool` (`Tool`, `ToolContext`). See ``crates/orbit-tools/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-exec`, `orbit-policy`, `orbit-types`.
- Depended on by: `orbit-cmd`, `orbit-core`, `orbit-engine`, `orbit-mcp`, `orbit-runtime`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-tools/src/lib.rs`.

Design intent: [docs/design/granted-tools/](../../docs/design/granted-tools/) — this page describes the code as it is; the design folder holds intent.
