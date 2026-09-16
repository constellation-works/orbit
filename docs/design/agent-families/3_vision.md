---
summary: "Agent Families — Vision"
type: design
title: "Agent Families — Vision"
owner: human
last_updated: 2026-08-09
last_validated: 2026-09-16
status: Draft
feature: agent-families
doc_role: vision
tags: ["agent-families"]
---

# Agent Families — Vision

The long-term direction is to keep family discovery small and explicit while moving experiment-friendly model selection into named crew configuration. Future work should reuse the registry instead of reintroducing hardcoded role defaults.

## 1. Open Questions

1. If cross-provider review returns, should it be an explicit review workflow rather than part of crew selection?
2. Should Orbit eventually version crew definitions so run-start can detect that a task pinned an older semantic lineup?
3. How much validation should occur at task authoring time versus run-start when remote config stores are introduced?

## 2. Prior Work

### Agent Family Onboarding

[ORB-00042] and follow-up tasks made `grok` a peer of `claude`, `codex`, and `gemini`. That work established the rule that adding a family is explicit and reviewed, not inferred from arbitrary strings.

### Role Defaults

Before [ORB-00058], default role models were split between `[agent.<role>]` config and hardcoded family pair resolution. That made model experiments global and temporary, which in turn made audit trails harder to interpret.

### Task Metadata

Task artifacts already supported optional fields that can be omitted from older YAML. The `crew` field follows that pattern so old tasks load while new tasks can express an intended lineup.

## 3. What May Be Distinctive

Orbit treats crew resolution as part of task execution provenance rather than just configuration lookup. A run records the exact resolved crew and model so later readers can understand what actually ran even after the workspace registry changes.

The distinction between family and crew also lets future workflows ask two different questions cleanly: "which executors are supported?" and "which concrete provider-model should this job use?"

## 4. References

- Orbit internal: [1_overview.md](./1_overview.md), [2_design.md](./2_design.md), [4_decisions.md](./4_decisions.md)
- Orbit internal: `crates/orbit-types/src/identity/agent_pair.rs`
- Orbit internal: `crates/orbit-core/src/runtime/engine/crew.rs`

## Task References

- ORB-00042: Onboard Grok (xAI) as a first-class supported agent family.
- ORB-00058: Introduce per-task crew override for agent model selection.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
