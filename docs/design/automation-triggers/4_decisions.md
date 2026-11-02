---
title: Automation Triggers — Decisions
owner: claude
last_updated: 2026-11-02
last_validated: 2026-11-02
status: Draft
feature: automation-triggers
doc_role: decisions
type: design
summary: Standing rules for automation triggers; an operator's `no-pilot` tag skips pilot admission holds.
tags: [automation-triggers, pilot]
paths: ["crates/orbit-core/src/application/task/pilot/**"]
related_features: [routines, user-interface]
related_artifacts: [ORB-21238]
---

# Automation Triggers — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

## `no-pilot` is an operator skip of pilot admission

**Recorded:** 2026-11 · [ORB-21238]

### Context

Editing a task's description or acceptance criteria after its pilot makes admission fail closed as `pilot_assessment_stale` until a new assessment completes — an `--approve-proposed` drain or scheduled discovery repilots it automatically when no live execution claim holds the task, or an operator requests one explicitly — ([design §6](2_design.md#6-pilot-eligibility-and-freshness)). When an operator has just settled the footprint and criteria themselves, as for a docs-only design task, that second pilot only adds delay. The `no-pilot` tag already existed but meant only "skip the first automatic pilot", and admission ignored it once a receipt existed.

### Decision

A task carrying `no-pilot` is not held by pilot assessment state at any admission path (drain, `orbit run ship`, pull admission, readiness). Admission skips the stale hold and every hold the latest receipt recorded: operator validation, host operational, native OS, duplicate and already-landed findings. The operator decided to dispatch without assessment, so the findings of an assessment they chose not to consult do not hold it either. The unverified review-finding hold stays: it is a review gate, not a pilot assessment. Readiness gaps, context locks, dependencies and delivery holds are other rules and are unchanged. Preparation eligibility keeps its meaning, and an explicit repilot still works on a tagged task.

Adding the tag to a task that has a pilot receipt or a pending pilot is operator authority. An attributed agent, a tool writer or a writer inside an agent envelope is refused with the whole write, never stripped of the tag. The operator's write appends a `pilot_skipped` history entry with the actor, the receipt's operation id and sealed digest, any pending pilot runs, and the requirements digest the write leaves, which is what dispatch proceeds on. Filing a task with the tag, or adding it before any pilot, stays open to every caller. Removing the tag restores the gate.

The dashboard Readiness card shows "Skip pilot" only while the drain holds a backlog task on one of those pilot reasons. It adds the tag through the ordinary guarded task update and re-reads the task's readiness.

### Consequences

- The stale-hold detail, `orbit run readiness` and the Readiness card name both remedies: `orbit task repilot <id>` and the `no-pilot` skip.
- A proposed task held on pilot warnings is approved as before; once it is in the backlog, admission does not hold it on those warnings.
- Cost: a skipped task can dispatch on requirements no pilot has read, including ones an earlier assessment flagged as duplicate or landed. The `pilot_skipped` entry is the only record of what was dispatched unassessed, so audits read history, not the receipt.
