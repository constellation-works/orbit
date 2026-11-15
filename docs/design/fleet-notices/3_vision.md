---
title: Fleet Notices — Vision
owner: claude
last_updated: 2026-11-02
last_validated: 2026-11-02
status: Draft
feature: fleet-notices
doc_role: vision
type: design
summary: "Open directions for fleet notices: the maintenance dispatch hold, real orchestrator credentials, multi-owner global notices, push delivery, and prior art"
tags: [fleet-notices]
paths: ["crates/orbit-core/src/application/notice/**"]
related_features: [fleet-notices, distributed-drain, operator-attention, path-leases]
related_artifacts: [ORB-21207]
---

# Fleet Notices — Vision

Forward-looking directions for [fleet notices](./1_overview.md). Nothing here is committed; v1 is
[2_design.md](./2_design.md). Speculation is labelled as such.

## 1. Open Questions

1. **Maintenance dispatch hold.** The contract is fixed in
   [2_design.md §7](./2_design.md#7-extension-point-maintenance-dispatch-hold). Open: should an
   in-flight pull drain also stop *requesting* claims on the owner side (the owner refusing claims
   from held hosts), so a follower on an older binary that cannot read holds is still held?
2. **Orchestrator credentials.** The `orchestrator` class rests on attribution. If session authority
   gains a real orchestrator credential, which notice rights should move behind it (closing others'
   notices, global posts)?
3. **Global notices across several owners.** A global notice reaches only its home owner's briefs.
   Should an owner be able to subscribe to another owner's global notices (an owner-to-owner read on
   the admission pass), and how would that fail when the other owner is unreachable?
4. **Push to running agents.** v1 delivers at prompt safe points. Is there a safe way to reach a
   long-running step mid-flight (for example via the same channel mid-run steering might gain), or is
   "next safe point" the right ceiling?
5. **Notice-aware filing.** Should `orbit.task.add` and `orbit.friction` action `add` warn when a new filing
   matches an open notice's linked task or dedupe key, so duplicates are caught at filing rather than
   in review?
6. **Acknowledgement.** Delivery is recorded, understanding is not. Would an execution-summary
   convention ("notices that affected this run: …") be enough evidence, or does anything need more?
7. **Kinds.** Are three kinds enough? A `resolved` announcement ("fixed, rebase your branches") is
   today an `info` notice linked to the fix with `close_on_linked_done: false`; watch whether that
   is awkward in use.
8. **Resident orchestrators.** Every resident cycle's brief carries notices, and the
   [resident-orchestrator](../resident-orchestrator/2_design.md#7-notices) design adds one reader-side
   rule: while a `maintenance` notice is open in scope, a resident narrows its own cycle to
   observe-only. The notice still holds no leaf dispatch. The manager posts as `orchestrator` and
   residents post as `agent`, under the existing poster classes. Watch whether that is enough, or
   whether the dispatch hold in question 1 should also read residents.

## 2. Prior Work

### Status pages and incident tooling

Public status pages (incident, maintenance, informational posts with a lifecycle) and incident
tools' "declare an incident" channels are the closest analogue: scoped, kinded, with explicit
resolution. Notices borrow the three kinds and the "maintenance window" idea. They differ in that the
readers are agents, so trust marking and posting authority matter more than presentation.

### Message-of-the-day and broadcast banners

Login banners and chat-ops "topic" lines are cheap broadcast with no lifecycle; they go stale, which
is why notices make expiry mandatory.

### Orbit's own neighbours

- [Path-lease notices](../path-leases/2_design.md#5-notices): an event stream on one lease, with a
  cursor. Fleet notices are records, not events; the names are kept apart in the glossary.
- [Decision requests](../human-decision-channel/1_scope.md): owner-held records with expiry derived
  on read; the precedent for effective state.
- [Mid-run steering](../mid-run-steering/1_overview.md): delivery into prompts at safe points with a
  delivery record; the precedent for brief delivery.
- [Operator attention](../operator-attention/1_overview.md): the read that lists open notices beside
  everything else waiting on an operator.

## 3. What May Be Distinctive

Possibly the combination of a broadcast channel whose readers are autonomous agents with posting
rights scaled to accountability (operator, orchestrator, run-bound agent), corroboration counts in
place of duplicates, and a standing rule that the channel informs but never authorizes. Each piece
exists elsewhere; whether the combination holds up in use is what v1 will show.

## 4. References

**Orbit-internal**

- [2_design.md](./2_design.md), [4_decisions.md](./4_decisions.md)
- [federated-mcp 2_design.md](../federated-mcp/2_design.md)
- [distributed-drain 2_design.md §7.4](../distributed-drain/2_design.md#74-host-shutdown-hold)

**External**

- None cited; the prior-work notes above are general practice rather than specific sources.

## Task References

- [ORB-21207] — designed fleet notices

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
