---
type: context
summary: Architecture map for orbit-agent: what it owns, main modules, dependencies and entry points.
tags: [architecture, orbit-agent]
---

# orbit-agent

## What it owns

One `AgentRuntime` per provider CLI (claude, codex, copilot, gemini, antigravity, grok, muse); audit types and redacted blob sinks.

## Main modules and types

`providers`, `runtime` (`AgentRuntime`), `agent` (`Agent`, `AgentConfig`), `loop_engine`, `types` (`AgentRequest`). See ``crates/orbit-agent/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-types`.
- Depended on by: `orbit-engine`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-agent/src/lib.rs`.

Design intent: [docs/design/agent-families/](../../docs/design/agent-families/) — this page describes the code as it is; the design folder holds intent.
