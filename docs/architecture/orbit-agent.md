---
type: context
summary: Architecture map for orbit-agent: what it owns, main modules, dependencies and entry points.
last_validated: 2026-11-25
tags: [architecture, orbit-agent]
---

# orbit-agent

## What it owns

One `AgentRuntime` per provider CLI (claude, codex, copilot, gemini, antigravity, grok, muse, ollama); audit types and redacted blob sinks.

## Main modules and types

`providers`, `runtime` (`AgentRuntime`), `agent` (`Agent`, `AgentConfig`), `loop_engine`, `types` (`AgentRequest`). See ``crates/orbit-agent/src/lib.rs``.

## Dependencies

- Depends on (internal, production): `orbit-common`, `orbit-types`.
- Depended on by: `orbit-engine`.

Enforced by `scripts/check-dependency-direction.sh`; table in `ARCHITECTURE.md`.

## Entry points

Start reading at `crates/orbit-agent/src/lib.rs`.

Design intent: [docs/design/agent-families/](../../docs/design/agent-families/) — this page describes the code as it is; the design folder holds intent.

## Ownership by module

- `agent`: Agent facade (`Agent`, `AgentConfig`, `ProviderOptions`) over provider runtimes; imports `runtime`, `types`, `orbit-common`, and `orbit-types`.
- `loop_engine`: Audit blob sinks for runtime persistence; imports `orbit-common`.
- `providers`: Provider CLI adapters and stdout projection helpers; imports `agent`, `runtime`, `types`, `orbit-common`, and `orbit-types`.
- `runtime`: Provider runtime trait (`AgentRuntime`), registry, and runtime factory; imports `agent`, `providers`, `types`, and `orbit-common`.
- `types`: Provider request, response-envelope, usage, and diagnostic contract types; imports `orbit-common` and `orbit-types` only.
