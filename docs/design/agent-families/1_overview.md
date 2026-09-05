---
summary: "Agent Families — Overview"
type: design
title: "Agent Families — Overview"
owner: grok
last_updated: 2026-08-09
last_validated: 2026-08-29
status: Draft
feature: agent-families
doc_role: overview
tags: ["agent-families"]
---

# Agent Families — Overview

Orbit models AI coding systems as first-class agent families and groups concrete provider-model assignments into named crews. Families answer "which provider/tooling is this model from?"; crews answer "which provider-model should run this task?"

## 1. Motivation

Orbit was initially built around three dominant agent CLIs: Claude Code, Codex (OpenAI), and Gemini. As more agents gained strong coding capabilities, including Grok via Grok Build and the xAI API, Orbit needed stable family identifiers for attribution, execution, review, and analytics.

Family defaults were originally also used to pick activity role models, but that made per-task model experiments require editing workspace config. The crew registry introduced by [ORB-00058] separates model selection from family discovery: workspaces define named assignments in `[crews.<name>]`, tasks may pin `crew`, and operators may override the crew for a single run.

## 2. Core Concepts

- **Agent Family:** A stable identifier such as `claude`, `codex`, `gemini`, or `grok` used for routing, attribution, and legacy model inference.
- **Model Inference:** `agent_from_model()` and `provider_from_model()` in `crates/orbit-types/src/identity/actor.rs`, together with `infer_agent_family_from_model()` in `crates/orbit-types/src/identity/agent_pair.rs`, map concrete model strings to families and providers. `infer_agent_family_from_model()` remains for legacy artifact recovery.
- **Crew:** A named provider-model assignment loaded from `.orbit/config.toml` under `[crews.<name>]`; every activity dispatch resolves to it.
- **Default Crew:** `[workflow].default_crew` names the workspace fallback when a task does not specify `crew`.
- **Executor:** A YAML definition in `crates/orbit-core/assets/executors/<name>.yaml` describing how to invoke an agent CLI. The file is named for the execution lane (`antigravity.yaml` for `agy`), which may differ from the model family (`gemini`).
- **Sandbox Surface:** Provider-specific state directories and lockfile rules required for safe `macos-sandbox-exec` execution.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Family inference and crew resolver | `crates/orbit-types/src/identity/actor.rs`, `crates/orbit-types/src/identity/agent_pair.rs` | [ORB-00042], [ORB-00058], [ORB-10130] |
| Task-level crew field | `crates/orbit-types/src/task/model.rs` | [ORB-00058] |
| Runtime config loading | `crates/orbit-config/src/resolved.rs` | [ORB-00058] |
| Default config template | `crates/orbit-config/assets/default-config.toml` | [ORB-00058] |
| Run-time crew resolution | `crates/orbit-core/src/runtime/engine/crew.rs` | [ORB-00058] |
| Tool and CLI crew surfaces | `crates/orbit-tools/src/builtin/orbit/task/` | [ORB-00058] |
| Grok family onboarding | `crates/orbit-types/src/identity/actor.rs`, `crates/orbit-types/src/identity/agent_pair.rs` | [ORB-00042] |

## Task References

- ORB-00042: Onboard Grok (xAI) as a first-class supported agent family.
- ORB-00043: Add Grok to agent_from_model, all_agent_families, and provider_from_model.
- ORB-00058: Introduce per-task crew override for agent model selection.
- ORB-10130: Flatten each crew to one provider-model assignment.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
