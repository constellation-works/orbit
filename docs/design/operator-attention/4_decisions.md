---
title: Operator Attention — Decisions
owner: claude
last_updated: 2026-11-08
last_validated: 2026-11-08
status: Draft
feature: operator-attention
doc_role: decisions
type: design
summary: "Decisions for orbit attention: derived view not inbox, stateless cursor, fingerprint scope, source-bound read rights, explicit caller polling, fan-out exception, blindness reporting, folding"
tags: [operator-attention]
paths: ["crates/orbit-core/src/application/attention/**", "crates/orbit-mcp/src/federated/**"]
related_features: [operator-attention, federated-mcp, human-decision-channel]
related_artifacts: [ORB-21164]
---

# Operator Attention — Decisions

Non-obvious decisions for [orbit attention](./1_overview.md), recorded by title. Rejected
alternatives are named in each entry. Task references carry provenance. See
[CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and the required
`Cost:` line. Code anchors name the proposed locations from [2_design.md §3](./2_design.md#3-placement-in-the-code);
slices confirm them.

## Attention is derived on read, not persisted as an inbox

**Recorded:** 2026-11 · [ORB-21164]

### Context

Operator-waiting work is spread across stores that already record it. A feed needs resolve,
ordering and "what changed" semantics. The tempting shape is an inbox: write an item when a
condition arises, mark it done when it clears, and let readers page it.

### Decision

When a surface aggregates facts that other stores own, derive it from those stores on every
read; do not copy the facts into a new store. `orbit.attention` has no item table, no
acknowledgement table and no cursor table.

Rejected:

- **A persisted inbox** written at event time. It needs every producer to remember to write
  and to close an item, so any missed close leaves a permanent false positive and any missed
  write a silent gap. It is a second system of record for each condition.
- **Event-bus subscription** feeding a store. Orbit has no event bus by design
  ([human-decision-channel](../human-decision-channel/1_scope.md)), and a store fed by one
  inherits the inbox's drift.
- **A cached snapshot refreshed on a schedule.** It is an inbox with a delay, and it returns
  resolved items until the next refresh.

### Consequences

- An item disappears when its source condition does, with no producer cooperation.
- A new surface (the dashboard) reads the same pure derivation.
- Cost: every call recomputes, there is no history of past items, and onset times exist only
  where a source records them. Acknowledge and snooze, which need state, are a separate
  decision ([3_vision.md](./3_vision.md#1-open-questions)).

## The delta cursor is stateless and carries what the caller has seen

**Recorded:** 2026-11 · [ORB-21164]
**Code anchors:** `crates/orbit-core/src/application/attention/cursor.rs` (proposed)

### Context

A caller needs only new or changed items on a later explicit read, without making the feed stateful.

### Decision

The cursor is an opaque token holding a set of hashes of the `(id, fingerprint)` pairs the
caller was shown, plus an issue time. Any process can read or mint one; Orbit stores nothing.
An unusable cursor (expired, malformed, unknown version) returns the full list, flagged, and
is never an error.

Rejected:

- **A server-side cursor or per-consumer acknowledgement table.** It is persisted state with
  its own garbage collection, cross-host replication problem and per-session identity, to
  answer a question the caller can carry itself.
- **A timestamp cursor** (`--since <time>`). It misses an item that changed in place
  (re-piloted with a new warning, a request re-raised) and depends on clocks agreeing across
  hosts.
- **A whole-list ETag.** It says "something changed" but cannot say which items.

### Consequences

- Cursors work across processes, restarts and hosts; sessions are independent by holding
  their own.
- Failing toward the full list means a caller can see an item twice but cannot miss one.
- Cost: a cursor grows with the list (up to about 10 KB), it cannot report resolved items
  (hashes do not decode to ids), and a consumer that wants a resolved feed must diff two full
  lists itself.

## The fingerprint covers state and severity, never wording or time

**Recorded:** 2026-11 · [ORB-21164]
**Code anchors:** `crates/orbit-core/src/application/attention/fingerprint.rs` (proposed)

### Context

"Changed" must mean "worth telling the reader again". Hashing the rendered item would re-inject
on every wording tweak and every `age_seconds` tick; hashing nothing but identity would never
re-inject a worsening item.

### Decision

The fingerprint hashes `{id, severity, per-kind state}`. State is a discriminator the source
already records (a pilot receipt's operation id, a run id, a claim's phase and diagnostics).
`severity` is included on purpose so age or an imminent expiry re-surfaces an item once.

Rejected:

- **Hash of the full item.** Churns on text and time.
- **A source-maintained revision counter.** Needs every source to grow one.
- **Identity only.** An item that got worse stays silent.

### Consequences

- Passing time is silent except through severity thresholds, which are deliberate nudges.
- Cost: every kind must name a state discriminator, and a source change that does not move the
  discriminator (an edited message under the same receipt) is invisible to deltas. New kinds
  carry a fingerprint definition or do not ship.

## A derived view never widens who may read its sources

**Recorded:** 2026-11 · [ORB-21164]

### Context

Some sources are operator-gated reads (`orbit.workflow.run.list`, `orbit.drain.claims`); others
are open to agents (`orbit.task.list`, `orbit.decision.list`). A combined tool could be gated
whole, which hides the task-level kinds from the orchestrators who need them, or run with
elevated rights, which hands them the gated reads.

### Decision

An aggregate shows an item only if the caller could have read its source directly. The tool
admits `Agent` and `Operator` sessions; each kind enforces its own source's capability and, when
refused, reports `degraded` with code `capability` for that kind. A new tool that combines
sources applies the same rule.

Rejected:

- **Operator-only tool.** Orchestrators would lose the feed.
- **Tool-level elevation** so the aggregate can read everything. An aggregate is the easiest
  place to leak a gated read, and the audit trail would credit the read to the tool.

### Consequences

- An agent session sees fewer kinds, and sees that it does.
- Cost: results differ by session capability, so two readers' lists are not interchangeable,
  and a consumer must surface `degraded`.

## Orbit ships the feed without a session hook

**Recorded:** 2026-11 · [ORB-21164]

### Context

Daniel’s final direction is explicit reads with a delta cursor. The session hook was dropped and [ORB-21165] is archived.

### Decision

Orbit provides the tool, CLI, filters and [cursor protocol](./specs/attention-cursor.md#caller-polling-protocol). No session hook, automation, notification, install command or live configuration change is authorized. The archived hook is neither a prerequisite nor a fifth slice.

Rejected:

- **An Orbit-shipped session hook** or `orbit attention install-hook`. It hard-codes one
  consumer's lifecycle events and state into core.
- **Consumer-flavoured kinds** (an "orchestrator-assignment-stale" kind). Those are one
  consumer's notion of attention; kinds must be conditions Orbit's own records define.

### Consequences

- Any harness (Claude, Codex, a cron job, a chat bot) can drive the same feed.
- Cost: each consumer writes a few lines and must follow the protocol; Orbit's only lever is
  documenting it well.

## Fan-out is a named exception in the federated mux

**Recorded:** 2026-11 · [ORB-21164]
**Code anchors:** `crates/orbit-mcp/src/federated/` (proposed)

### Context

[federated-mcp](../federated-mcp/2_design.md) routes each workspace-scoped call to the one
destination a host-qualified selector names, and treats a bare call as `unknown_selector`. One
call from a follower covering the owner's workspaces needs the mux to call several
destinations and merge. Its non-goals exclude silent merging of host-local state.

### Decision

A tool the contract names as a fan-out read may be called without a selector; the mux calls
every in-scope destination in parallel and merges, attributing each item to its host and
surfacing every failure. `orbit.attention` is the only such tool in v1. The destination is
authoritative for what it reports: an owner reports task-level kinds, a replica reports only
its own runs and health, and no caller substitutes a replica's copy for an unreachable owner.

Rejected:

- **Client-only fan-out**, leaving MCP callers to loop over selectors. It works for the CLI,
  but an agent session would spend a call per workspace and each agent would re-implement the
  merge and the unreachable-host handling.
- **A global tool reading follower replicas.** It reports stale replica task state as current.
- **An owner-side central aggregator** that followers push to. It adds a write path and a
  store, which decision one rules out.

### Consequences

- One call covers the fleet, with hosts visible on every item.
- Cost: the mux gains its first merging behaviour, so federated-mcp's "no aggregate" wording
  is amended with this exception; the exception must stay one named tool unless a new decision
  widens it.
- A later repair keeps `host`, `all`, and `workspace` (including a host-qualified selector) on
  that fleet read. A session binding routes to one checkout only when the call names none of
  those. An owner of a named workspace who was not a destination is `degraded`, and an empty
  message names the hosts and workspaces actually read. See
  [attention-federation](./specs/attention-federation.md).

## An aggregate that cannot see a source says so

**Recorded:** 2026-11 · [ORB-21164]

### Context

The motivating failure is things going unnoticed. An empty list that really means "the owner
was unreachable" would be worse than no tool.

### Decision

When a source, kind or host cannot be read, the response says so in `degraded` and the
empty-result message does not claim all-clear. An unreachable destination is also an item
(`host_unhealthy`) so it can be seen, sorted and filtered like any other. `degraded` is not
filterable by kind: excluding `host_unhealthy` hides the item but never the fact of blindness.
The same rule applies to any later feed Orbit builds over several sources.

Rejected:

- **Omit what cannot be read** and log it. This is the silent gap the feature exists to close.
- **Fail the whole call** when any source fails. One flaky host would blank the list for the
  rest of the fleet.

### Consequences

- The operator sees the fleet's blind spots in the same place as its problems.
- Cost: a flaky link produces recurring `host_unhealthy` and `degraded` noise, and consumers
  must handle a non-empty `degraded` beside real items.

## Open decision requests fold into the item they decide

**Recorded:** 2026-11 · [ORB-21164]
**Code anchors:** `crates/orbit-core/src/application/attention/derive.rs` (proposed fold step)

### Context

A pilot hold, an escalated block and a settled review each can carry an open decision request.
Listing the hold and its request as two rows double-counts one waiting condition and makes
`--kind` answers depend on whether the decision channel is enabled.

### Decision

Fold a request into the item of the state it decides; show it as `refs.decision_id` and
`expires_at` and make its answers the item's `next`. `decision_open` lists only requests with
no subject item (`agent_request`, `proposed_approval`). When the request expires or is
superseded, the subject item remains with a decision-free `next`.

Rejected:

- **One row per source record**, the plain union. It is simpler but doubles rows and changes
  counts when the decision channel is toggled.
- **Dropping the subject item** when a request exists. It would hide the hold's own reason
  when the request expires unanswered.

### Consequences

- Counts mean "things waiting", independent of which mechanism holds them.
- Cost: `--kind decision_open` does not list every open request; `orbit decision list` stays
  the complete list of requests, and a reader who wants requests by gate uses it.

## Task References

- [ORB-21164] — recorded the attention design decisions
- [ORB-21165] — archived session-hook proposal

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
