---
title: Workspace Agent Corpus — Overview
type: design
owner: Orbit
created_date: 2026-10-31
updated_date: 2026-11-28
status: Draft
stage: shipped
next: "—"
feature: workspace-agent-corpus
doc_role: overview
summary: "Pinned activity-scoped corpus guidance and deterministic append proposals"
tags: [workspace-agent-corpus]
---

# Workspace Agent Corpus — Overview

Agent activities receive reviewed, activity-specific corpus guidance pinned to the run's base
revision, and their structured observations are kept for later reviewed publication. Corpus
injection, append proposals and decision-rule consolidation are implemented. The precedent
auto-answer repairs [ORB-20822] and [ORB-20823] are done; the documents remain Draft pending
recorded design approval. The mechanisms are in [2_design.md](./2_design.md); decisions that
qualify are kept in [4_decisions.md](./4_decisions.md).

## 1. Motivation

### Problem

Agent activities otherwise start without the workspace's reviewed issue mechanisms,
decision precedent and standing guidelines. Raw task history alone cannot serve as
trusted guidance or authorize a new decision.

### Goal

Deliver reviewed, activity-specific corpus guidance at a pinned revision and preserve
structured observations for later reviewed publication, with human curation of rules.

Activity dispatch already pins repository guidance to a run's base revision, and task
history supplies the durable append outbox. This feature joins those existing seams.

### Non-goals

- Reintroducing the retired project-learnings inference store.
- Treating raw decision history or agent narrative as trusted rules or automatic appends.
- Reading or writing the orbit-office Library.

### Phasing

1. **Corpus, pinned injection and append outbox:** done in [ORB-20637].
2. **Historical corpus backfill:** done in [ORB-20650].
3. **Curated decision rules and opt-in auto-answer:** done in [ORB-20747]. The
   merged-source and exact-topic repairs are done in [ORB-20822] and [ORB-20823];
   replay/routing and chronology repairs are done in [ORB-21053] and [ORB-21064].
4. **Portable corpus proposals:** done in [ORB-22229], retaining every-group proposals
   where the workspace supplies the mapped corpus and script.

### Acceptance (feature level)

- Dispatch uses pinned, exact-activity corpus rows and respects size bounds and refusals.
- Only trusted structured producers create idempotent task-history append proposals;
  reviewed publication precedes injection.
- Only current, matching, operator-auto precedent can answer a new request; raw history
  and unmerged corpus edits cannot supply that authority.
- Consolidation produces proposal text without refreshing authority from replayed history
  or replacing a newer curated choice with an older answer.

## 2. Core Concepts

- **Corpus.** The reviewed documents a workspace keeps under `docs/agents/corpus/`:
  `INDEX.md` plus `issues/`, `decisions/` and `guidelines/`. Nothing is enabled by default.
- **Index row.** A generated line in `INDEX.md` naming one document and the exact activities
  it `applies_to`. Dispatch injects only the rows that name the dispatched activity.
- **Pinned injection.** Delivery of those rows as read at the run's recorded base commit.
  Task fields and worktree edits never supply injected text.
- **Append outbox.** The system `agent_corpus_append` task-history entry written when a
  human answers a request or a final recovery is applied. It is durable in task history
  and is not injected until a reviewed batch pull request lands.
- **Curated rule.** A `current` decision rule in the `## Rules` section of a topic file,
  built from human citations. Raw decision history is never a rule.
- **Precedent auto-answer.** A current rule the operator has set to `auto: true` answers a
  new request that names it exactly, read at the workspace's pinned integration revision.

## 3. At a Glance

| Concern | File | Task |
| --- | --- | --- |
| Corpus layout, document schema and generated index | [2_design.md §1–2](./2_design.md#1-corpus-layout) · [agent-corpus.py](../../../scripts/agent-corpus.py) | [ORB-20637] |
| Pinned exact-activity injection and size bounds | [2_design.md §3](./2_design.md#3-pinned-injection) · [corpus_outbox.rs](../../../crates/orbit-core/src/app/agent/corpus_outbox.rs) · [preparation.rs](../../../crates/orbit-engine/src/activity_job/cli_runner/orchestrator/dispatch/provider/preparation.rs) | [ORB-20637] |
| Injection base revision and integration revision for precedent | [base_revision.rs](../../../crates/orbit-core/src/application/task/base_revision.rs) | [ORB-20637], [ORB-20822] |
| Append outbox from human answers and final recovery | [answering/record.rs](../../../crates/orbit-core/src/application/task/decision/answering/record.rs) · [final_recovery/apply.rs](../../../crates/orbit-core/src/application/task/final_recovery/apply.rs) | [ORB-20637] |
| Reviewed batch materialization | [2_design.md §4](./2_design.md#4-deterministic-writes-and-reviewed-batches) · [agent-corpus.py](../../../scripts/agent-corpus.py) | [ORB-20637] |
| Historical backfill of blocked and escalated tasks | [2_design.md §4](./2_design.md#4-deterministic-writes-and-reviewed-batches) | [ORB-20650] |
| Pilot-block-patterns corpus proposals | [2_design.md §4](./2_design.md#4-deterministic-writes-and-reviewed-batches) | [ORB-22229], [ORB-25391], [ORB-25426] |
| Curated rule parsing, consolidation and staleness | [rules.rs](../../../crates/orbit-core/src/application/task/decision/rules.rs) · [consolidate.rs](../../../crates/orbit-cli/src/command/decision/consolidate.rs) | [ORB-20747], [ORB-21053], [ORB-21064] |
| Precedent auto-answer, operator-only `auto` and reversal | [precedent.rs](../../../crates/orbit-core/src/application/task/decision/precedent.rs) · [raise.rs](../../../crates/orbit-core/src/application/task/decision/raise.rs) | [ORB-20747], [ORB-20823] |

## Task References

- [ORB-20637] — implements the activity-scoped corpus, pinned injection and the deterministic append outbox.
- [ORB-20650] — backfills the corpus from the past month of blocked and escalated tasks.
- [ORB-20747] — adds weekly decision-rule consolidation, staleness checks and opt-in auto-answer.
- [ORB-20822] — pins precedent auto-answer rules and path digests to the workspace integration revision.
- [ORB-20823] — removes the topic-file-stem match from precedent auto-answer; a bare rule id must appear in exactly one decision file.
- [ORB-21053] — prevents replay from refreshing rule authority and routes unique bare ids to existing rules.
- [ORB-21064] — preserves newer curated choices during partial-history replay.
- [ORB-22229] — conditions pilot-block-patterns corpus proposals on the mapped corpus and script.
- [ORB-25391] — moves new corpus issue documents to specifically scoped follow-up tasks.
- [ORB-25426] — derives follow-up paths from the mapped corpus directory.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
