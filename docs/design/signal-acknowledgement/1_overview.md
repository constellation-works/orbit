---
title: Signal Acknowledgement — Overview
owner: claude
created_date: 2026-11-23
updated_date: 2026-11-28
last_validated: 2026-11-24
status: Accepted
stage: designing
next: "Daniel promotes ORB-27754 from someday to start phase 1; phases 2 and 3 are unfiled"
feature: signal-acknowledgement
doc_role: overview
type: design
summary: "An operator-only `orbit ack` that marks an attention item, degraded source, doctor warning or run warning as expected, with reason, author and a required expiry, shown as acknowledged and left out of counts"
tags: [signal-acknowledgement, operator-attention]
---

# Signal Acknowledgement — Overview

Bearing: [`orbit attention`](../operator-attention/1_overview.md) derives every item from
existing state and has no acknowledgement table
([decision](../operator-attention/4_decisions.md#attention-is-derived-on-read-not-persisted-as-an-inbox));
its vision left acknowledge and snooze open
([3_vision.md](../operator-attention/3_vision.md#1-open-questions), question 1). Two
narrow switches already exist: `execution.env.login_auth` mutes the unset-credential warning
for a provider that keeps its own login ([configuration reference](../../config/README.md)), and `orbit host
offline --reason … [--until …]` lists a deliberately offline host once under the attention
envelope's `acknowledged` field instead of as `host_unhealthy` (ORB-27669). Doctor rows have
a stable `check_name`; drain and ship warnings are named fields of `SubmissionNotices`.

## Problem

Agents keep raising the same expected signal, and Daniel answers it again each time. On
2026-11-23 he named two: dk-server-1 being down and the Claude API key warning in drains. Each
has a dedicated switch now. The rest do not. A drain warning, a doctor warning or an attention
item that is known and accepted looks exactly like a new one. No surface says "the operator
has seen this, here is why it is fine, until when". The formation trial keeps these answers as
scribe entries outside Orbit, so other providers and sessions never see them.

## Goal

An operator records once that a signal is expected, with a reason and an expiry. Every Orbit
surface that would raise it shows it as acknowledged (reason, by, until), inline and without
any outside lookup, and leaves it out of its counts. This holds for every provider and session
reading that host or workspace. A critical signal is never hidden. Sources, severities and
`next` actions stay as they are; an acknowledgement changes only how a signal is shown and
counted.

## Non-goals

- **Hiding critical signals.** Excluded by rule, not by setting.
- **Kind-wide or pattern acknowledgements.** A kind-wide acknowledgement such as "ignore all
  `pilot_warning`" changes what counts as a condition, which is code and spec
  ([attention 2_design §5](../operator-attention/2_design.md#5-configuration)).
- **Snooze in the cursor.** A per-caller "seen" set already exists in the stateless cursor.
  An acknowledgement is shared state, not one caller's memory.
- **Changing behaviour.** An acknowledged warning still has its effect: a drain still excludes
  a crew without credentials.

## Acceptance (feature level)

- Given an active acknowledgement of a non-critical attention item, `orbit attention` lists it
  only under `acknowledged_items` with reason, by and until, and leaves it out of every count.
  When it expires, its fix task closes or its fingerprint changes, the item returns to `items`
  and to the delta feed.
- A `critical` item or a doctor `error` row that matches an acknowledgement stays in its list,
  its count and the exit status, annotated.
- A doctor warning row and a drain warning that match an acknowledgement show as acknowledged
  and are not counted as warnings, on every provider's session on that host or workspace.
- An agent or resident session that calls `orbit.ack.add` is refused.

The mechanisms are in [2_design.md](./2_design.md); decisions that qualify are kept in [4_decisions.md](./4_decisions.md).

## Task References

- ORB-27669 — added the deliberate host-offline mark that attention lists as acknowledged

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
