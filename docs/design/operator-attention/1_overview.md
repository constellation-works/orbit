---
title: Operator Attention — Overview
owner: claude
last_updated: 2026-11-04
last_validated: 2026-11-04
status: Draft
stage: shipped
next: "—"
feature: operator-attention
doc_role: overview
type: design
summary: "orbit attention: one read-only, filterable, federated view of everything waiting on an operator, with a stateless delta cursor for callers"
tags: [operator-attention, operator-ux, decision-channel, federated-mcp]
paths: ["crates/orbit-core/src/application/attention/**", "crates/orbit-common/src/governance/**", "crates/orbit-cli/src/command/**", "crates/orbit-mcp/src/federated/**"]
related_features: [human-decision-channel, federated-mcp, host-registry, distributed-drain, mcp-session-context]
related_artifacts: [ORB-21164, ORB-21165]
---

# Operator Attention — Overview

`orbit attention` is one read-only list of every item in the workspaces a host reaches that is
waiting on an operator: pilot warnings and holds, blocked tasks, open decisions, failed or
stuck runs, stale claims, reviews awaiting a verdict, a red base branch, untriaged friction,
unhealthy hosts, open fleet notices and stalled resident mandates. It is derived from existing state on every call (no inbox, no new
store), every item names the exact command that resolves it, and a stateless cursor lets a
caller request only what is new or changed. The CLI is `orbit attention`; the tool is
`orbit.attention`.

## 1. Motivation

Work that needs an operator is recorded in many places, each with its own query:
`orbit task list --pilot-warnings`, `orbit task list --status blocked`, `orbit decision list`
(held decisions only), `orbit run history`, `orbit drain claims`, `orbit friction list`,
`orbit host list`, `orbit doctor`. Nothing asks the combined question, so items go
unnoticed until someone opens the dashboard.

On 2026-11-01 four proposed tasks ([ORB-21114], [ORB-21118], [ORB-21127], [ORB-20363]) carried
pilot warnings that hold promotion, and ORB-20363 also carried a contract-mismatch
finding. None surfaced until Daniel saw them in the UI. [ORB-21164] records his ask: "a
holistic summary of issues that requires operator attention", available to orchestrators,
initially injected through a hook, and filterable (for example by workspace). Daniel subsequently dropped the session hook; [ORB-21165] is archived and it is no dependency or deliverable. He chose the name
`attention`. The existing channel work ([human-decision-channel](../human-decision-channel/1_scope.md))
solves the last mile for decisions it can email; this feature covers everything else and
gives the orchestrator one place to look.

## 2. Core Concepts

- **Item**: one thing waiting on an operator, with a closed `kind`, a `severity`, the ids it
  concerns, a `since` time, a one-line `reason` quoted from its source, a `next` action and a
  `fingerprint`. Schema: [attention-item](./specs/attention-item.md).
- **Kind**: the closed enum of reasons (`pilot_warning`, `pilot_hold`, `blocked`,
  `decision_open`, `run_failed`, `run_stuck`, `claim_stale`, `review_waiting`, `ci_red`,
  `friction_untriaged`, `host_unhealthy`, `notice_open`, `resident_stalled`). Each kind has exactly one source of truth that
  already exists.
- **Derived view**: the list is rebuilt from the sources on each call. An item exists
  exactly while its source condition holds; resolving the source removes the item.
- **Fingerprint**: a stable hash that changes only when an item materially changes. It is
  what makes deltas possible.
- **Cursor**: an opaque, self-contained token that records which `(item, fingerprint)`
  pairs a caller has already been shown. Passing it back as `--since` returns only the rest.
  Contract: [attention-cursor](./specs/attention-cursor.md).
- **Aggregator**: the layer that merges per-workspace results across workspaces and hosts,
  applies the cursor, and turns unreachable hosts into items. Contract:
  [attention-federation](./specs/attention-federation.md).
- **Explicit reads**: operators and agents call the feed and retain a cursor when useful. No session hook, automation, notification or live config change is part of this feature.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Kinds, sources, item schema, severity, `next` | [specs/attention-item.md](./specs/attention-item.md) | [ORB-21164] |
| Filters and their combination semantics | [specs/attention-filters.md](./specs/attention-filters.md) | [ORB-21164] |
| Fingerprint, `--since` cursor, expiry | [specs/attention-cursor.md](./specs/attention-cursor.md) | [ORB-21164] |
| Multi-workspace, multi-host, unreachable hosts | [specs/attention-federation.md](./specs/attention-federation.md) | [ORB-21164] |
| How the pieces fit, code placement, slices | [2_design.md](./2_design.md) | [ORB-21164] |
| Rejected alternatives and standing rules | [4_decisions.md](./4_decisions.md) | [ORB-21164] |
| Deferred directions (ack/snooze, dashboard, notifications, plugin kinds) | [3_vision.md](./3_vision.md) | [ORB-21164] |

All four slices ([ORB-21168], [ORB-21169], [ORB-21170], [ORB-21171]) have landed; [ORB-21215] added `notice_open` and [ORB-21245] added `resident_stalled`;
[2_design.md §6](./2_design.md#6-slices) orders them.

## Task References

- [ORB-21164] — designed `orbit attention` and filed its implementation slices
- [ORB-21168] — slice 1: tool, CLI, filters and cursor over the task-level kinds
- [ORB-21169] — slice 2: runtime kinds
- [ORB-21170] — slice 3: `ci_red` and `friction_untriaged`
- [ORB-21171] — slice 4: federation and `host_unhealthy`
- [ORB-21165] — archived the session-hook proposal; it is not a dependency or deliverable
- [ORB-21215] — added the open-notice attention kind
- [ORB-21245] — added the stalled-resident attention kind
- [ORB-21114] — carried a pilot warning in the motivating example
- [ORB-21118] — carried a pilot warning in the motivating example
- [ORB-21127] — carried a pilot warning in the motivating example
- [ORB-20363] — carried a pilot warning and contract finding in the motivating example

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
