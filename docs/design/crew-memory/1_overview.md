---
title: Crew Memory — Overview
owner: claude
last_updated: 2026-11-08
last_validated: 2026-11-08
status: Draft
stage: building
next: "M1–M4 are built behind the disabled-by-default gate; next run the M5 memory-on vs memory-off experiment"
feature: crew-memory
doc_role: overview
type: design
summary: "Experimental: storage, gated memory delivery, weekly dreaming and operator review of global-memory proposals are built. Adoption depends on the memory-on vs memory-off experiment."
tags: [crew-memory, experiments, agent-scorecards]
paths: ["crates/orbit-core/src/application/crew_memory/**", "crates/orbit-core/src/application/experiment/**", "crates/orbit-engine/src/activity_job/cli_runner/**"]
related_features: [agent-scorecards, experiments, auto-tasks, project-learnings]
related_artifacts: [ORB-23105, ORB-23113, ORB-23114, ORB-23115, ORB-23116, ORB-10736]
---

# Crew Memory — Overview

Crew memory lets each model learn from its own recent work. Each model has a **memory space**: a few short Markdown files. M1 builds the store and operator write surfaces; M2 adds an experiment overlay that captures the drawn model's memory, and delivers it in the agent envelope only for a live memory arm. Shadow arms capture what they would have delivered for the readout. **The feature is experimental and off by default.** M3's weekly, score-blind **dreaming run** rewrites model spaces, and M4 lets operators review **global-memory** proposals. Adoption depends on the memory-on versus memory-off [experiment](../experiments/1_scope.md): the memory arm must do better without breaching a guardrail, or the feature is removed.

## 1. Motivation

The [agent scorecards](../agent-scorecards/2_design.md) measure every role every week, and the weekly crew-rankings note publishes the result. The M2 overlay provides a gated path for memory to reach an agent, but its value is not established and it remains off by default. Outside an enabled live arm, the same kinds of shortfall recur from week to week, and a model starts each task knowing nothing of its last hundred. The existing manual fix is for an orchestrator to notice a pattern and edit a tag instruction, a skill or a repository guide; that applies to every model alike and provides no measurement of whether it helped.

Orbit has tried durable learnings before. The native project-learning subsystem was retired by [ORB-10736] because automatic delivery of scoped learnings "did not justify" its store, lifecycle, surfaces and curation machinery. Crew memory differs in three ways, each recorded in [4_decisions.md](./4_decisions.md):

- It has to prove its value in a randomised experiment before it is adopted.
- It is written from scorecard evidence by the model it serves, not curated by hand.
- It reuses the experiment overlay and admission-receipt seams instead of adding its own search corpus, dashboard or hooks.

## 2. Core Concepts

- **Memory key.** The model a space belongs to, as the scorecards report it (`row model`). Crews of one model share a space: `opus` and `opus@medium` read and write the same memory. This matches how the weekly ranking pools crews by model.
- **Memory space.** A model's memory: up to 8 Markdown files, together at most 6 KiB. The model decides what the files hold and how they are organised. Orbit enforces only the caps, the provenance rule and the content lint.
- **Global memory.** One space shared by every model in the workspace, at most 3 KiB. Operators list proposals with evidence and display-only similarity, accept (optionally editing the whole global text) or reject with a reason. Open proposals expire after four weeks.
- **Revision.** Every write replaces a space's files whole and appends an immutable revision, which records the files' content digest, the evidence task ids the write cited, the run that wrote it and the time.
- **Dreaming run (M3).** A weekly auto-task per memory key, run by a crew of that model. It receives a frozen, score-blind **evidence pack** and writes its own space, plus at most two promotion proposals.
- **Promotion proposal (M4).** A dreaming run's request to add a note to the global memory, with supporting evidence and the proposing model, for an operator to accept or reject.
- **Memory overlay (built in M2).** An experiment overlay surface. In arms that enable it, the owner resolves the drawn crew's space plus the global memory at admission. It renders them as a framed data block and captures the block, with its digest, in the admission receipt. A live arm delivers the block; a shadow arm records what it would have delivered.

## 3. At a Glance

Slices M1–M4 are built. M1 provides the store, write rules and operator CLI; M2 provides the memory overlay, capture and envelope delivery, all behind `crew_memory.enabled` (default `false`). Only a live memory arm delivers notes to an agent; shadow arms capture the would-be delivery for readout. M3 adds dreaming and M4 adds global proposal review and expiry. M5 runs and concludes the experiment.

| Concern | Source or design | Task |
|---------|------|------|
| Spaces, revisions, proposals (owner store) | `crates/orbit-store/src/driver/sqlite/migration/crew_memory.rs`, `crates/orbit-store/src/driver/sqlite/crew_memory_store.rs` | [ORB-23113] |
| Write validation: caps, provenance, lint | `crates/orbit-core/src/application/crew_memory/validate.rs` | [ORB-23113] |
| Memory key and the one write path | `crates/orbit-core/src/application/crew_memory/key.rs`, `operations.rs` | [ORB-23113] |
| Memory overlay, receipt capture and dispatch resolution | `crates/orbit-core/src/application/experiment/definition.rs`, `crates/orbit-core/src/application/experiment/capture.rs`, `crates/orbit-core/src/application/experiment/overlay.rs`, `crates/orbit-core/src/application/crew_memory/delivery.rs`, `crates/orbit-engine/src/activity_job/cli_runner/envelope.rs` | [ORB-23114] |
| Evidence pack from the scorecards | `crates/orbit-core/src/application/agents/memory_evidence.rs` | [ORB-23115] |
| Weekly dreaming routine | `crates/orbit-core/src/application/crew_memory/dream.rs` | [ORB-23115] |
| Operator CLI (`orbit memory show\|history\|export\|write\|reset\|copy`) | `crates/orbit-common/src/governance/memory.rs`, `crates/orbit-cli/src/command/memory.rs` | [ORB-23113] |
| Agent tools (`orbit.memory.*` for dreaming runs) | `crates/orbit-core/src/application/crew_memory/agent.rs` | [ORB-23115] |
| Global-memory proposal review | `crates/orbit-core/src/application/crew_memory/proposals.rs` | [ORB-23116] |
| Memory-on vs memory-off experiment and conclusion (planned M5) | `docs/design/crew-memory/2_design.md` §7 | — |

## Task References

- [ORB-23105] — designs crew memory (this folder)
- [ORB-23113] — builds slice M1: store, write rules, operator CLI
- [ORB-23114] — builds slice M2: memory overlay, capture, envelope delivery and readout strata
- [ORB-23115] — builds slice M3: dreaming, evidence packs and scoped agent tools
- [ORB-23116] — builds slice M4: global proposal review and expiry
- [ORB-10736] — retired the native project-learning subsystem

M5 runs and concludes the memory-on vs memory-off experiment; adoption remains undecided.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
