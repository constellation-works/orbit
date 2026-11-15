---
title: Resident Orchestrator — Overview
owner: claude
last_updated: 2026-11-08
last_validated: 2026-11-08
status: Draft
stage: building
next: "Re-file the slice 10 pilot (ORB-21251 archived 2026-11-03, not done) once a bounded mandate with an operator authority grant and a manager session is ready"
feature: resident-orchestrator
doc_role: overview
type: design
summary: "One manager session is the human's front door; each mandate gets a resident orchestrator that runs as bounded, checkpointed Orbit activity cycles on the existing clock, coordination only"
tags: [resident-orchestrator, operator-attention, human-decision-channel, routines, fleet-notices]
paths: ["crates/orbit-core/src/application/resident/**", "crates/orbit-core/assets/jobs/**", "crates/orbit-core/assets/activities/**", "crates/orbit-types/src/workflow/automation/members.rs"]
related_features: [routines, condition-triggers, activity-job, human-decision-channel, operator-attention, fleet-notices, mid-run-steering, decision-assistance]
related_artifacts: [ORB-21229, ORB-12488, ORB-21164, ORB-21207, ORB-21178]
---

# Resident Orchestrator — Overview

A **resident orchestrator** is the loop that drives one orchestration mandate, run by Orbit
rather than held in an open chat. It is an Orbit activity, not a daemon and not a session. Each
**fire** comes from the clock or from a change in the state the mandate watches. A fire reloads
the mandate's assignment task, runs one bounded **cycle** (check, recover, dispatch, promote,
merge, only within the authority the human granted), writes a structured **checkpoint** and
exits. When nothing has changed, a fire is a deterministic no-op that starts no model. In front
of the residents sits one **manager**: an interactive session of any provider and the human's
only conversational surface. It files and scopes mandates, relays the human's grants and
directions verbatim, posts fleet notices and summarises progress. It holds no loop and no state
that Orbit does not hold. Leaf work stays on the existing drain and leaf pipeline, unchanged.
The shape is the almanac note's `1:1:many:many`: one human, one manager, many residents, many
leaf executors.

This folder is a rewrite from scratch. Orbit's earlier resident-orchestrator design owned
execution: an epic worktree, a pinned drain slot and its own admission path. It was removed with
the epic pipeline in [ORB-12488]. [4_decisions.md](./4_decisions.md#coordination-only-the-resident-owns-no-execution)
records that design, why it failed and how this one differs.

## 1. Motivation

Today every orchestration mandate lives in an open interactive chat. ORB-21178 is an example: the
scribe v2 delivery in ws_orbit-office. Orbit holds the durable record, the `[orchestration]`
assignment task, but the loop that drives it is improvised in the chat:

- self-scheduled wakeups;
- monitors that expire every 30 minutes;
- ssh polling scripts.

A closed laptop, a crash or a context compaction stops the loop, and the operator has to keep one
chat open per mandate. Execution and orchestration are reliable at this scale; ownership is not.
Daniel (2026-11-02): "I just need to talk to a single \"manager\" and manager delegates notices,
gets update, etc to the resident orchestrator. I don't need to have orchestrator chat sessions
opened."

Two things push this into Orbit rather than into a constellation script. First, the parts already
exist: the clock and routines ([routines](../routines/1_overview.md)), typed state triggers and
condition triggers ([condition-triggers](../condition-triggers/1_scope.md)), the activity and job
machinery ([activity-job](../activity-job/1_overview.md)), the human decision channel
([human-decision-channel](../human-decision-channel/1_scope.md)), fleet notices
([fleet-notices](../fleet-notices/1_overview.md)) and `orbit attention`
([operator-attention](../operator-attention/1_overview.md)). A resident is a composition of those,
plus a mandate record, a checkpoint contract and an authority boundary. Second, authority has to be
enforced by core, not by a prompt: a resident that may dispatch but not merge must be unable to
call the merge tool.

Abstracting orchestration away has a cost the design also has to pay: the pain that drove Orbit's
recent improvements (`orbit attention`, fleet notices) was felt by a human across sessions. A
resident that quietly works around a gap hides it. The evolution loop
([2_design.md §11](./2_design.md#11-the-evolution-loop)) keeps that signal.

## 2. Core Concepts

- **Manager** — the human's single conversational front door: an interactive session of any
  registered crew that files mandates, relays the human's words, posts notices and reads residents'
  state. It never holds a loop and cannot widen authority
  ([2_design.md §1](./2_design.md#1-roles)).
- **Mandate** — the assignment task a resident works for: goal, exit criteria, deadline, covered
  workspaces, crew and cadence, held as a versioned artifact on an `[orchestration]` assignment
  task ([specs/mandate-and-authority.md](./specs/mandate-and-authority.md)).
- **Authority** — the rights a mandate grants (`dispatch`, `promote`, `merge`, `plugin_upgrade`)
  over its covered workspaces. Only the human widens it; anyone may narrow it.
- **Resident** — the mandate's loop, run by Orbit as cycles. It has state (`active`, `paused`,
  `taken_over`, `ended`) but no process between cycles.
- **Fire** — the clock tick at which a mandate is due: its wake digest changed or its heartbeat
  elapsed, never faster than its cadence floor.
- **Cycle** — one bounded run of the `resident_cycle_pipeline` job: observe, one agent loop,
  finalize ([2_design.md §5](./2_design.md#5-the-cycle)).
- **Checkpoint** — the structured `resident.checkpoint` comment a cycle leaves on the assignment
  task. The next cycle reads it; nothing reads the agent's final message
  ([specs/checkpoint-contract.md](./specs/checkpoint-contract.md)).
- **Escalation** — a question the resident must not answer itself, raised to the human through the
  decision channel ([2_design.md §8](./2_design.md#8-escalation)).
- **Cost event and evolution record** — a structured note of a workaround or manual step, and the
  per-signature record that counts them across residents; the manager digest reads them
  ([specs/evolution-ledger.md](./specs/evolution-ledger.md)). The evolution pass that turned
  records into proposals was removed in [ORB-22844].

Terms are defined in [references/glossary.md](./references/glossary.md).

## 3. At a Glance

Slices 1–9 ([ORB-21242] to [ORB-21250]) have landed; slice 10, the pilot ([ORB-21251]), was archived on 2026-11-03 without running and is not done.
Planned code locations are in [2_design.md §12](./2_design.md#12-placement-in-the-code).

| Concern | File | Task |
|---------|------|------|
| Mandate record, authority, grants and narrowing | [specs/mandate-and-authority.md](./specs/mandate-and-authority.md) | [ORB-21242] |
| Checkpoint, request and answer shapes; resume | [specs/checkpoint-contract.md](./specs/checkpoint-contract.md) | [ORB-21243], [ORB-21246] |
| Fires, the cycle job, authority-scoped tools | [2_design.md §4–§5](./2_design.md#4-lifecycle) | [ORB-21244] |
| Manager ↔ resident channel, control, `resident` toolset | [specs/manager-channel.md](./specs/manager-channel.md) | [ORB-21245] |
| Metrics that gate widening | [2_design.md §9](./2_design.md#9-metrics-that-gate-widening) | [ORB-21247] |
| Cost events, evolution records, digest (evolution pass removed [ORB-22844]) | [specs/evolution-ledger.md](./specs/evolution-ledger.md) | [ORB-21248], [ORB-21249] |
| Manager and resident skill references | [2_design.md §10](./2_design.md#10-migration-and-pilot) | [ORB-21250] |
| Pilot against an interactive baseline | [2_design.md §10](./2_design.md#10-migration-and-pilot) | [ORB-21251] |
| Removed design, rejected alternatives, standing rules | [4_decisions.md](./4_decisions.md) | [ORB-21229] |
| Open questions and prior work | [3_vision.md](./3_vision.md) | [ORB-21229] |

## Task References

- [ORB-21229] — rewrote the resident-orchestrator design from scratch and filed its slices
- [ORB-12488] — removed the epic pipeline and the earlier resident-orchestrator design
- [ORB-21178] — the interactive scribe v2 orchestration used as the baseline (ws_orbit-office mandate)
- [ORB-21164] — designed `orbit attention`, where residents report what waits on the human
- [ORB-21207] — designed fleet notices, which every resident's brief reads
- [ORB-21242] — slice 1: mandate record and operator-only authority grant
- [ORB-21243] — slice 2: checkpoint tool and request/answer shapes
- [ORB-21244] — slice 3: `resident_due` trigger, cycle job and authority-scoped tools
- [ORB-21245] — slice 4: manager channel and the `resident` toolset
- [ORB-21246] — slice 5: brief reconstruction and the provider-continuity adapter
- [ORB-21247] — slice 6: `orbit resident metrics`
- [ORB-21248] — slice 7: cost events and the evolution record kind
- [ORB-21249] — slice 8: evolution pass and manager digest
- [ORB-21250] — slice 9: manager and resident skill references
- [ORB-21251] — slice 10: the pilot mandate (archived 2026-11-03, not done; re-file once a bounded mandate with an operator authority grant and a manager session is ready)
- [ORB-22844] — removed the evolution pass routine, job and proposal filing; the ledger and manager digest stay

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
