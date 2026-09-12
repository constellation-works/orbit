---
summary: "Agent Families — Design"
type: design
title: "Agent Families — Design"
owner: human
last_updated: 2026-08-09
last_validated: 2026-08-29
status: Draft
feature: agent-families
doc_role: design
tags: ["agent-families"]
---

# Agent Families — Design

This document describes the current implementation of Orbit agent families and crew-based model assignment. It covers the family registry, workspace config surfaces, task and CLI override surfaces, and where resolved run metadata is persisted.

## 1. Family Registry

The family registry spans `crates/orbit-types/src/identity/actor.rs` and `crates/orbit-types/src/identity/agent_pair.rs`. `all_agent_families()` returns the supported family identifiers; `agent_from_model()` and `provider_from_model()` infer family and provider from model strings in `actor.rs`; and `infer_agent_family_from_model()` in `agent_pair.rs` remains the conservative recovery helper for older persisted artifacts.

Adding a family is still a cross-cutting change: executor assets, sandbox behavior, provider inference, review automation, and scoreboard code all need review. The fixed registry forces that audit instead of silently accepting unknown families.


## 2. Crew Registry

Workspace config defines one concrete assignment under each `[crews.<name>]`: flat `model` and `provider` fields. A rendered activity input may select a named `crew`; without one, dispatch uses the run's resolved crew. Activity and job schemas reject the retired `role` key.

`crates/orbit-config/src/raw.rs` owns the TOML shape, and `crates/orbit-config/src/resolved.rs` materializes it into `Crew` values from `orbit-types`. Runtime loading rejects incomplete crews, retired `planner`/`implementer`/`reviewer` role sub-tables with guidance to write flat `model` and `provider` fields, and `[workflow].default_crew` values that do not name a defined crew. A retired `backend` key is accepted only as inert `cli` or rejected with migration guidance.

The built-in runtime registry uses model-specific standard crews: Claude provides `opus`, `sonnet`, and `fable`; Codex provides `sol`, `terra`, and `luna`; Antigravity provides `antigravity`; Gemini CLI still provides the legacy `gemini` crew; Grok provides `grok`; Copilot provides `copilot`; Cursor provides `cursor`; and Pi provides `pi`. Fresh `orbit init` config filters that registry by detected provider CLIs and chooses the first emitted standard crew as `[workflow].default_crew` (`opus`, `astra`, `antigravity`, `gemini`, `grok`, `copilot`, `cursor`, or `pi`).

It adds `qa` on Terra when Codex is available, otherwise on Sonnet when Claude is available. With no supported provider CLI, initialization emits neither crews nor a dangling default.

`copilot`, `cursor`, `pi`, and `antigravity` are crews named for their *provider* rather than their model: those lanes can select models supplied by other vendors, so model-named crews would hide which execution lane actually runs. Antigravity (`agy`) is the current Google terminal CLI; `gemini` remains the model family and the legacy Gemini CLI lane. See [CONFIG.md § Antigravity CLI](../../CONFIG.md#antigravity-cli). [ORB-10946] [ORB-10945] [ORB-11299]

## 3. Task and Tool Surface

`Task` has an optional `crew` field. `orbit.task.add` and `orbit.task.update` validate authored crew names against the current workspace registry, and an `orbit.task.update` transition to `in_progress` accepts a one-run `crew` override. The runtime re-validates at start time because the config registry can change between task creation and execution.

The precedence chain is:

1. CLI/tool start override `crew`
2. `Task.crew`
3. `[workflow].default_crew`

This chain resolves the run crew. At activity dispatch there is one additional, explicit authoring choice: a rendered `crew` input selects a different named crew for that activity. With no such input, the run crew is the fallback. No role-keyed lookup participates in either selection.

`orbit.task.show` surfaces the task field and, when the current registry resolves it, the effective crew name plus one `crew_model` string. Crew configuration is host-local, so a read surface never fails on a crew this host cannot resolve: the stored `crew` is returned verbatim, `resolved_crew`/`crew_model` are withheld, and `crew_unresolved` carries the reason as a non-fatal warning [ORB-10968]. Listing and the global id lookup follow the same contract; `orbit.task.update` transitions to `in_progress` and dispatch still resolve strictly and fail with the crew-validation error.

## 4. Run Records

Run-start code resolves the crew before dispatch, emits structured tracing fields for `resolved_crew` and `crew_model`, and persists those strings on the job run record. Persisting resolved values protects audit trails from later config edits.

Legacy records without crew fields still deserialize because the run-record fields are optional. Display code may use `infer_agent_family_from_model()` only as a recovery path for older artifacts.

## 5. Concerns & Honest Limitations

Crew names are workspace-local strings. Renaming or deleting a crew leaves a task naming the old one unable to *run* here, though it stays readable and existing run records keep the resolved model strings.

An activity-specific crew is carried in rendered input rather than persisted as separate per-step routing metadata. Authors must ensure that input is visible in the job asset when a step intentionally differs from its run.

## Task References

- ORB-00042: Onboard Grok (xAI) as a first-class supported agent family.
- ORB-10946: Add the standalone GitHub Copilot CLI as a first-class workflow executor.
- ORB-10945: Add Cursor Agent CLI as a first-class Orbit workflow executor.
- ORB-00058: Introduce per-task crew override for agent model selection.
- ORB-10315: Seed model-specific crews only for providers available during initialization.
- ORB-10620: Reject retired crew role sub-tables during config load.
- ORB-10622: Remove activity roles and make the run crew the activity fallback.

Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
