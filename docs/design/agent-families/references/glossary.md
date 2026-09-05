---
type: design
summary: "Agent Families — Glossary"
tags: ["agent-families"]
last_validated: 2026-09-05
---

# Agent Families — Glossary

**agent family** — A stable identifier (`claude`, `codex`, `gemini`, `grok`) representing a coherent set of models, CLIs, and integration requirements that Orbit treats uniformly for attribution, execution, and analytics.

**model pair** — The legacy `(orchestrator, helper)` model duo represented by `AgentModelPair`. User-authored executor definitions may retain `model_pair_override` for compatibility; shipped defaults use crew-selected models and `model_flag` instead.

**all_agent_families()** — The single source of truth function in `orbit-common` that returns the fixed-size array of supported families. Changing its size is intentionally high-friction.

**executor** — The YAML definition (`crates/orbit-core/assets/executors/<name>.yaml`) that describes how `backend: cli` invokes an agent's CLI. The name is the execution lane (`antigravity` for `agy`), which is not always the model family (`gemini`).
