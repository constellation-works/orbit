---
title: Crew Memory — Overview
owner: claude
last_updated: 2026-11-08
last_validated: 2026-11-08
status: Draft
stage: building
next: "M1 (store, write rules, operator CLI) landed with ORB-23113; next is M2, the memory overlay surface verified in shadow"
feature: crew-memory
doc_role: overview
type: design
summary: "Experimental: each model keeps a small, capped, evidence-cited memory delivered into its own prompts, rewritten weekly by a score-blind reflection run (dreaming); crew-wide notes go to a reviewed global memory; adopted only if a memory-on vs memory-off experiment shows value."
tags: [crew-memory, experiments, agent-scorecards]
paths: ["crates/orbit-core/src/application/crew_memory/**"]
related_features: [agent-scorecards, experiments, auto-tasks, project-learnings]
related_artifacts: [ORB-23105, ORB-23113, ORB-10736]
---

# Crew Memory — Overview

Crew memory lets each model learn from its own recent work. Each model gets a **memory space**: a few short Markdown files that it writes and that Orbit delivers into that model's own prompts: implementer prompts first, with reviewer and task-pilot prompts once those roles have experiments of their own. Once a week, a **dreaming run** per model reads the evidence behind that model's shortfalls and rewrites the model's space. The evidence covers escaped regressions, rework, review misses and pilot misjudgements, with the tasks behind each count; it never includes scores or ranks. When a lesson holds across models, the dreaming run proposes it for the **global memory**, which changes only when an operator accepts the proposal. **The feature is experimental.** It is off by default, and memory reaches a prompt only through the memory arm of a running [experiment](../experiments/1_scope.md). It is adopted only if that experiment's readout shows the memory arm does better without breaching a guardrail. If it doesn't, the feature is removed.

## 1. Motivation

The [agent scorecards](../agent-scorecards/2_design.md) measure every role every week, and the weekly crew-rankings note publishes the result. Nothing feeds that result back to the agents. The same kinds of shortfall recur from week to week, and a model starts each task knowing nothing of its last hundred. Today the only fix is manual: an orchestrator notices a pattern, then edits a tag instruction, a skill or a repository guide. That is slow, it applies to every model alike, and nobody measures whether it helped.

Orbit has tried durable learnings before. The native project-learning subsystem was retired by [ORB-10736] because automatic delivery of scoped learnings "did not justify" its store, lifecycle, surfaces and curation machinery. Crew memory differs in three ways, each recorded in [4_decisions.md](./4_decisions.md):

- It has to prove its value in a randomised experiment before it is adopted.
- It is written from scorecard evidence by the model it serves, not curated by hand.
- It reuses the experiment overlay and admission-receipt seams instead of adding its own search corpus, dashboard or hooks.

## 2. Core Concepts

- **Memory key.** The model a space belongs to, as the scorecards report it (`row model`). Crews of one model share a space: `opus` and `opus@medium` read and write the same memory. This matches how the weekly ranking pools crews by model.
- **Memory space.** A model's memory: up to 8 Markdown files, together at most 6 KiB. The model decides what the files hold and how they are organised. Orbit enforces only the caps, the provenance rule and the content lint.
- **Global memory.** One space shared by every model in the workspace, at most 3 KiB. It changes only when an operator accepts a promotion proposal.
- **Revision.** Every write replaces a space's files whole and appends an immutable revision, which records the files' content digest, the evidence task ids the write cited, the run that wrote it and the time.
- **Dreaming run.** A weekly auto-task per memory key, run by a crew of that model. It receives a frozen, score-blind **evidence pack** and may write only its own space, plus at most two promotion proposals.
- **Promotion proposal.** A dreaming run's request to add a note to the global memory. It carries the supporting evidence and the proposing model, and an operator accepts or rejects it.
- **Memory overlay.** A new experiment overlay surface. In arms that enable it, the owner resolves the drawn crew's space plus the global memory at admission. It renders them as a framed data block and captures the block, with its digest, in the admission receipt.

## 3. At a Glance

Slice M1 is built: the store, the write rules and the operator CLI, behind `crew_memory.enabled` (default `false`). Nothing delivers memory to a prompt yet. Rows marked planned are where later slices will live.

| Concern | File | Task |
|---------|------|------|
| Spaces, revisions, proposals (owner store) | `crates/orbit-store/src/driver/sqlite/migration/crew_memory.rs`, `crates/orbit-store/src/driver/sqlite/crew_memory_store.rs` | [ORB-23113] |
| Write validation: caps, provenance, lint | `crates/orbit-core/src/application/crew_memory/validate.rs` | [ORB-23113] |
| Memory key and the one write path | `crates/orbit-core/src/application/crew_memory/key.rs`, `operations.rs` | [ORB-23113] |
| Memory overlay surface and receipt capture | `crates/orbit-core/src/application/experiments/` (planned) | [ORB-23105] |
| Evidence pack from the scorecards | `crates/orbit-core/src/application/agents/` (planned) | [ORB-23105] |
| Dreaming auto-task definition | `.orbit/auto_tasks/crew-memory-dream.yaml` (planned, plugin-seeded) | [ORB-23105] |
| Operator CLI (`orbit memory show\|history\|export\|write\|reset\|copy`) | `crates/orbit-common/src/governance/memory.rs`, `crates/orbit-cli/src/command/memory.rs` | [ORB-23113] |
| Agent tools (`orbit.memory.*` for dreaming runs) | `crates/orbit-common/src/governance/memory.rs` (planned) | [ORB-23105] |

## Task References

- [ORB-23105] — designs crew memory (this folder)
- [ORB-23113] — builds slice M1: store, write rules, operator CLI
- [ORB-10736] — retired the native project-learning subsystem

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
