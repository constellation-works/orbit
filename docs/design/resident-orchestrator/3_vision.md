---
title: Resident Orchestrator — Vision
owner: claude
last_updated: 2026-11-02
last_validated: 2026-11-02
status: Draft
feature: resident-orchestrator
doc_role: vision
type: design
summary: "Open questions (cross-owner mandates, many managers, resident-to-resident coordination, cost policy), prior work and what is distinctive about bounded resident orchestration"
tags: [resident-orchestrator]
related_features: [resident-orchestrator, federated-mcp, owner-failover, decision-assistance]
related_artifacts: [ORB-21229]
---

# Resident Orchestrator — Vision

What the committed design in [2_design.md](./2_design.md) leaves open, what it draws on, and what
may be distinctive about it. Nothing here is committed scope.

## 1. Open Questions

1. **Cross-owner mandates.** v1 residents cover only workspaces owned by their own host. A mandate
   that spans dk-server-2 and the Mac's `ws_orbit-research` needs federated writes with the same
   authority enforcement on the remote owner. Until then such a mandate is two residents and a
   human (or the manager) joining them.
2. **Widening to many managers.** The `1:many:many:many` step waits on the metrics in
   [2_design.md §9](./2_design.md#9-metrics-that-gate-widening). Open questions there: does each
   manager own a domain of mandates, or are managers interchangeable views? How does the human see
   one digest across them?
3. **Resident-to-resident coordination.** Two mandates that touch the same files are a
   cross-mandate conflict, which v1 escalates. A later version could let residents see each other's
   footprints (their dispatched tasks' `context_files`) and sequence without the human.
4. **Cost policy as data.** "Material cost" is a fixed escalation class with a workspace-defined
   threshold. Orbit has usage pressure ([usage-pressure](../usage-pressure/1_scope.md)) but no
   spend budget per mandate. A budget would let the rubric weigh slot time against question delay
   in the same unit.
5. **Owner failover.** A resident runs on its workspace's owner. When the owner moves
   ([owner-failover](../owner-failover/1_overview.md)), the mandate's lease and cadence need to move
   with it. The stateless cycle makes this mostly free; the lease handover is not designed.
6. **Precedent-answered escalations.** A current corpus rule the operator marked auto can answer
   a decision request that names it ([workspace-agent-corpus](../workspace-agent-corpus/1_scope.md));
   model-answered categories were removed with decision assistance in [ORB-22808]. Whether
   resident escalations may be answered from precedent is a human call, made after the reversal
   metric has data.
7. **A dashboard view.** Residents appear through checkpoints, attention items and the digest. A
   dedicated view (timeline of cycles, open requests, costs) is likely worth it after the pilot.

## 2. Prior Work

### Orbit-internal

- **The removed resident-orchestrator design** (PR #630, PR #934, [ORB-10815]), recorded in
  [4_decisions.md](./4_decisions.md#coordination-only-the-resident-owns-no-execution). It showed
  that an orchestrator that owns execution taxes every other path.
- **Routines and the stateless clock** ([routines](../routines/1_overview.md)): the reason residents
  can exist without a daemon.
- **Task-pilot**: an existing model of a bounded, state-triggered, checkpointed agent pass whose
  output is durable task state.
- **Mid-run steering** ([mid-run-steering](../mid-run-steering/1_overview.md)): the precedent for
  delivering human words through the durable record at a safe point.

### Agent orchestration elsewhere

- **Supervisor and hierarchical multi-agent patterns** (manager agents delegating to worker agents
  in common agent frameworks). They usually keep the hierarchy in one process and one context;
  here each level is a separate, restartable unit joined only by durable records.
- **Workflow engines with durable execution** (Temporal-style workflows, Kubernetes controllers'
  reconcile loops). The resident cycle is a reconcile loop: observe desired state (the mandate) and
  actual state (Orbit), take bounded actions, record, exit. The difference is that the reconciler
  is a model working under an explicit authority grant.

### The almanac discussion

Daniel's note `from-layered-agent-scaling-to-bounded-resident-orchestration.md` (almanac,
2026-08) frames the `1:1:many:many` shape, the provider-neutral roles (its §10) and the
escalation equation. It lives in a separate repository, cited here by name.

## 3. What May Be Distinctive

- **The reconcile loop is a model, but correctness is not in the model's memory.** Every cycle can
  start cold from Orbit records, so a provider switch, a crash or an expired conversation is an
  ordinary event.
- **Authority is a record with an asymmetric write rule.** Narrowing is free and widening needs the
  human, so the relay layer (the manager) cannot become a privilege-escalation path.
- **The system measures its own pain.** Cost events and evolution records turn "I had to do this by
  hand again" into ranked evidence, and the human, not the system, decides what to build from it.

## 4. References

### Orbit-internal

- [routines](../routines/1_overview.md), [condition-triggers](../condition-triggers/1_scope.md),
  [activity-job](../activity-job/1_overview.md)
- [human-decision-channel](../human-decision-channel/1_scope.md),
  [decision-assistance](../_archived/decision-assistance/1_scope.md) (archived)
- [operator-attention](../operator-attention/1_overview.md),
  [fleet-notices](../fleet-notices/1_overview.md)
- [distributed-drain decisions](../distributed-drain/4_decisions.md#epic-is-a-tag-not-a-pipeline)

### External

- Almanac (separate repository): `15-discussions/26-08/from-layered-agent-scaling-to-bounded-resident-orchestration.md`.

## Task References

- [ORB-21229] — rewrote the resident-orchestrator design
- [ORB-10815] — revised the removed design to its v2 epic-worktree form

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
