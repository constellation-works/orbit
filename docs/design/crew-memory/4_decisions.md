---
title: Crew Memory — Decisions
owner: claude
last_updated: 2026-11-08
last_validated: 2026-11-08
status: Draft
feature: crew-memory
doc_role: decisions
type: design
summary: "Why crew memory is experimental until a randomised readout favours it, score-blind, own-space-only, keyed by model, and held in the owner store rather than host files."
tags: [crew-memory]
paths: ["crates/orbit-core/src/application/crew_memory/**"]
related_features: [experiments, project-learnings, agent-scorecards]
related_artifacts: [ORB-23105, ORB-10736]
---

# Crew Memory — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

## Experimental until a randomised readout favours it

**Recorded:** 2026-11 · [ORB-23105]

### Context

Orbit retired native project learnings ([ORB-10736]) because their value was asserted rather than measured, and the machinery to deliver them cost more than they returned. Memory that agents write about themselves can also make things worse: it can bloat prompts, harden into one-off habits, or teach a model to avoid risk. A before-and-after comparison can't separate any of that from the week-to-week noise in the scorecards.

### Decision

A mechanism that changes what an agent is told about its own past work ships **off by default**. It reaches prompts only as an experiment overlay, and it becomes default behaviour only when a concluded experiment's readout favours it on the primary metric with no guardrail breached. When the readout doesn't, the mechanism is removed rather than left dormant, following the [ORB-10736] precedent. Until then, a workspace setting can't turn memory on outside an experiment arm.

How this differs from project learnings:
1. Value is measured before adoption, by randomised assignment of tasks.
2. Content is written by the model it serves, from scorecard evidence, not curated by hand.
3. Delivery reuses the experiment overlay and admission receipt. There is no new search corpus, dashboard, hook installer or context-matching engine.

### Consequences

- Every memory delivery has an assignment row and an overlay digest, so a readout can say exactly what each task received.
- A failed readout ends with code removed, not with a disabled feature left behind.
- Cost: the experiment needs weeks of throughput before any operator benefit, and the weekly dreaming runs spend tokens throughout, all of which are charged to the memory arm's cost.
- Cost: the feature can't be tried informally on one workspace "just to see". Every trial is a formal experiment.

## Reflection is score-blind

**Recorded:** 2026-11 · [ORB-23105]
**Code anchors:** `crates/orbit-core/src/application/crew_memory/evidence.rs::build_pack` (planned)

### Context

The dreaming run exists because of the scorecards, but the scores are a proxy. A model that learns how its scorecard is computed has cheap ways to raise it that make the work worse. A reviewer can reject more and wave through less. An implementer can take smaller, safer diffs and avoid the risky changes the task needs.

### Decision

A reflection or memory-writing run receives the work behind each problem, never the measurement of it:

- **Included:** the tasks, review findings, regression fixes, rework and pilot misjudgements behind each nonzero problem count.
- **Excluded:** scores, ranks, rates, weights, fleet averages, expected counts and the other models' rows.

Memory content is linted on write and refused when it refers to the scorecard itself. This applies to any future agent-facing feedback loop built on the scorecards.

### Consequences

- The model can't tell which shortfall costs the most points, so it weighs lessons by the work, not by the score.
- The experiment's `lines_changed` and `escaped_regressions` guardrails catch the risk-avoidance failure the lint can't see.
- Cost: the lint is lexical and produces false positives on legitimate words ("rank" in a ranking feature). A refused write must be rephrased.
- Cost: hiding the weights throws away a real signal about which problems the operator cares about most.

## A model reads only its own space and the reviewed global memory

**Recorded:** 2026-11 · [ORB-23105]

### Context

Memory written by one model and delivered into another's prompt is an injection channel. One model's mistake or adversarial phrasing would become another model's instruction, with no human in the loop.

### Decision

A run receives exactly two spaces: its own model's space and the global memory. Text written by one model reaches another model's prompt only after an operator accepts it into the global memory. Every delivered block is framed as data, below the task's instructions and the repository guidance, and the framing says those take precedence over it.

### Consequences

- Peer learning between models goes only through promotion proposals, so the operator sees every cross-model note.
- Cost: lessons that apply across models spread at the speed of operator review, and an unattended week promotes nothing.

## Memory is keyed by model, not crew

**Recorded:** 2026-11 · [ORB-23105]
**Code anchors:** `crates/orbit-core/src/application/crew_memory/key.rs::memory_key` (planned)

### Context

Crews are model lanes with an effort setting: `opus`, `opus@medium` and `opus@high` are one model. Per-crew spaces would split one model's evidence three ways, and each third would often fall below the scorecard's minimum support. The weekly ranking already pools crews by model for the same reason.

### Decision

The memory key is the model that the scorecard row reports. Every crew of that model reads and writes one space, and one dreaming run per model writes it from the pooled evidence of all its crews. When a crew's configured model changes, a new key starts empty. Carrying a space over to a new model is an operator action.

### Consequences

- Cost: lessons that only apply at one effort level are mixed with the rest, and the note has to say which effort level it is about.
- Cost: a model upgrade starts from an empty space unless the operator copies the old space across.

## Memory lives in the owner store and reaches followers through the admission receipt

**Recorded:** 2026-11 · [ORB-23105]
**Code anchors:** `AdmissionReceipt` memory capture (planned, next to `tag_instructions`)

### Context

The obvious layout is a directory of Markdown files (`.orbit/memory/global.md`, `.orbit/memory/<model>/…`). But `.orbit/` is per host and untracked ([ORB-12718]). A follower's worker would read a stale copy or none at all, and edits leave no revision, digest or provenance.

### Decision

Spaces, revisions and proposals are rows in the owner's store (`~/.orbit/orbit.db`, keyed by workspace). The owner resolves memory at admission and captures the rendered block and its digest in the admission receipt. A follower applies what the claim carries, and a claim in a memory arm that lacks the capture is refused. Files on disk are only an export (`orbit memory export`), never read back.

### Consequences

- Every host's workers see the same memory, and a readout can tie each run to the exact revision it got.
- Cost: an operator can't fix a note by editing a file. Every change goes through the CLI or MCP and is validated and recorded.

## Task References

- [ORB-23105] — designs crew memory (this folder)
- [ORB-10736] — retired the native project-learning subsystem
- [ORB-12718] — kept `.orbit/` untracked, including leftover learnings

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
