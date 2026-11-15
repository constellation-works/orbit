---
title: Operator Attention — Vision
owner: claude
last_updated: 2026-11-04
last_validated: 2026-11-11
status: Draft
feature: operator-attention
doc_role: vision
type: design
summary: "Deferred directions for orbit attention: acknowledge and snooze, dashboard feed, notifications, plugin kinds, resolved feed, approval kind"
tags: [operator-attention, vision]
paths: ["crates/orbit-core/src/application/attention/**"]
related_features: [operator-attention, human-decision-channel, plugins, user-interface]
related_artifacts: [ORB-21164]
---

# Operator Attention — Vision

This document records what v1 does not build and what the design deliberately keeps open.
The remaining questions are speculative; the committed scope is in
[2_design.md](./2_design.md).

## 1. Open Questions

1. **Acknowledge and snooze.** An operator who has seen an item and chosen to leave it will see
   it again at the next full list. Silencing it needs state (who, until when, for which
   fingerprint). That is a store, so it is a separate decision that must answer where the state
   lives, whether it replicates, and whether an acknowledgement is bound to a fingerprint (so a
   material change un-silences it). The v1 design does not preclude it: ids are stable and
   fingerprints exist, so an acknowledgement table could key on `(id, fingerprint)` and be
   joined into the derivation without changing the item schema.
2. **Dashboard reading the same feed.** The derivation is a pure function of a runtime and a
   filter ([2_design.md §3](./2_design.md#3-placement-in-the-code)), so a dashboard endpoint
   can call it. Open: whether the dashboard wants severity-grouped views or live updates, and
   whether it needs the resolved feed (question 4).
3. **Notifications.** Pushing an item to a person (email, chat, desktop) is the decision
   channel's job for decisions ([human-decision-channel](../human-decision-channel/1_scope.md))
   and could consume the delta feed for the rest. Open: whether a notifier polls the
   cursor like any other consumer (the likely answer) without adding a session hook.
4. **A resolved feed.** The cursor cannot name resolved items. A consumer that wants "this
   was handled" must diff full lists. A short-lived resolved ledger would be state, so it is
   deferred with question 1.
5. **Plugin-contributed kinds.** v1 is a closed enum so that severity, fingerprint and `next`
   stay Orbit's to define and tested. A plugin could later register a source that returns items
   in the same schema under a namespaced kind (`plugin:<name>/<kind>`), subject to the same
   read-rights rule ([4_decisions.md](./4_decisions.md#a-derived-view-never-widens-who-may-read-its-sources))
   and required to supply its own fingerprint state. The wire `kind` is already a string with
   a documented closed set, so allowing namespaced values is additive. **Deferred, not in
   v1**: it needs the plugin standard's callback model to carry a read-only source and a
   timeout budget first ([plugins](../plugins/1_scope.md)).
6. **Plain proposed approvals.** Proposed tasks awaiting approval are the normal state of
   most backlog intake, so v1 omits them. If the decision channel's digest ([ORB-21091] family)
   becomes the standard approval path, an `approval_waiting` kind sourced from raised
   `proposed_approval` requests would be natural; today those requests already appear as
   `decision_open` when raised.
7. **Run progress.** A heartbeat on job runs would let `run_stuck` mean "stalled" rather than
   "owner gone or too long". That belongs to the run store, not to attention.
8. **Change reason in deltas.** The cursor cannot tell "new" from "changed" because it holds
   hashes. If consumers want that distinction, the cursor would need ids, trading size for a
   `change` field.
9. **Resident orchestrators as a source.** The rewritten
   [resident-orchestrator](../resident-orchestrator/2_design.md#6-the-manager--resident-channel)
   design reports through attention: resident escalations are `decision_open`, handoffs
   `review_waiting`, and it added one kind, `resident_stalled` (no checkpoint for twice the
   heartbeat, three incomplete cycles in a row, or a disabled workspace cycle routine),
   implemented by [ORB-21245]. The manager session reads
   attention instead of keeping an inbox.

## 2. Prior Work

### Orbit-internal

- `orbit task list --pilot-warnings`, `orbit decision list`, `orbit friction list`,
  `orbit host list`, `orbit doctor`, and `orbit.drain.claims` each answer one slice of the
  question. Attention composes them and adds no source of its own.
- The human decision channel ([human-decision-channel](../human-decision-channel/2_design.md))
  raises per-hold requests with expiry and answers; attention is the reading side that spans
  everything, not only what the channel can email.
- The federated workspace list's rule that unreachable destinations are included, not omitted
  ([federated-mcp](../federated-mcp/2_design.md)), is reused for hosts as items.

### External

Inbox and alert systems (pager queues, notification centers) persist alerts and acknowledge
them; log-based "what changed since" feeds use opaque resume tokens. Attention takes the
resume-token idea and rejects the persisted inbox (decisions one and two).

## 3. What May Be Distinctive

Little in the parts. The distinctive combination is a derived-only feed whose delta is carried
by the caller, so a many-host, many-agent system gets "tell me only what changed" without a
shared store.

## 4. References

- Orbit-internal: [human-decision-channel](../human-decision-channel/1_scope.md),
  [federated-mcp](../federated-mcp/2_design.md), [host-registry](../host-registry/specs/host-routing.md),
  [plugins](../plugins/1_scope.md).
- External: none.

[ORB-21164] recorded these deferred directions.

## Task References

- [ORB-21245] — implemented the `resident_stalled` attention kind
- [ORB-21164] — recorded the deferred directions
- [ORB-21091] — added decision-request risk tier and category

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
