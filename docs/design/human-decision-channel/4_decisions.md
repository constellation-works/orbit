---
title: Human decision channel — Decisions
owner: claude
last_updated: 2026-11-07
last_validated: 2026-11-07
status: Draft
feature: human-decision-channel
doc_role: decisions
type: design
summary: "Decisions: a decision's effect runs on the host that owns its request, a plugin's effect is never written under the human's label, and requests that decide agent memory are answered only by a human"
tags: [human-decision-channel, plugins, decision-assistance]
paths: ["crates/orbit-core/src/application/task/decision/**", "crates/orbit-types/src/task/decision/request/**"]
related_features: [plugins, decision-assistance, application-approvals, state-compatibility]
related_artifacts: [ORB-22824]
---

# Human decision channel — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

The three entries below were proposed with plugin-raised decision requests
([2_design.md](./2_design.md) §12) in [ORB-22824]. They bind once Daniel accepts that
design. All three are standing rules (Door 2): each governs the next gate, plugin or
host this channel gains, not only the scribe case that prompted it.

## A decision effect runs on the host that owns its request

**Recorded:** 2026-11-07 · [ORB-22824]

### Context

A decision request is task history, so it lives on the host that owns the task's
workspace. Answering it is a coordination write that a replica checkout or claimed
worker is refused (`coordination_task_write_refusal`). A plugin's effect must run where
the plugin and its data are. These can be different hosts: scribe's store is per
host and the one that matters holds Daniel's Mac sessions, while scribe's own
workspace (ws_scribe), the channel and `ws_orbit` are owned by dk-server-2. The real
alternative was a forwarded apply. The owner host records the human's answer, then asks the plugin's
host to run the effect over a host-to-host route. That would keep one channel for
every host. It would also move delegated-human authority across hosts and add a
cross-host write path with its own failure modes (partition, a lost outcome, double
apply).

### Decision

Raise a request only on a task whose effect can run on the same host, and refuse the
raise when it cannot. Never forward an answer or an apply to another host. When work
on one host needs a decision whose effect belongs on another, move the request: raise
it on the effect's host, against a task in a workspace that host owns. Do not move
the effect. This applies to every future gate, plugin and channel.

### Consequences

- Every decision's authority record, apply and audit sit in one host's history. A
  partition delays a decision and never splits it.
- A producer runs where its data is: scribe's consistency routine must be enabled in a
  workspace the Mac owns to raise requests about the Mac's store.
- Cost: a host without a registered channel has no email reach. scribe proposals
  raised on the Mac are answerable only on the Mac's dashboard, CLI and in-session
  `#scribe approve` until the Mac registers its own channel and `decision-mail`
  install. Today one mailbox cannot serve
  requests on every host.

## A plugin's effect is never written under the human's label

**Recorded:** 2026-11-07 · [ORB-22824]

### Context

When a human answers a plugin request, something must carry the answer into the
plugin's effect. The alternative was to mint the apply call's callback session with
the human's writer label, so the plugin's callback writes read as the human's (the
direction ORB-22824's brief first sketched). That would make plugin-authored history
entries indistinguishable from human ones, and it would pass every guard that keys off
a non-automation writer, `operator_resolution` among them. That is the gap §3.1
closed for transports. A plugin's callback sequence is plugin code: the human approved
an outcome, not each write the plugin chooses to make on the way.

### Decision

Core records the human's authority exactly once, on the `decision_answered` entry it
writes itself. The writer is the human, with channel provenance. Any effect a plugin
or other non-core code performs on that authority keeps its own provenance and runs at
a ceiling the operator consented to. It carries a `decision` provenance field naming
the request and the answerer, never the human's writer label. This applies to every
future mechanism that runs non-core code on a human's answer.

### Consequences

- Audit reads in two steps, "daniel answered on the dashboard, then plugin scribe applied
  under dr-…", and a human-writer guard can never be satisfied by plugin code.
- The apply ceiling is a separate, explicit operator consent (the apply-tool allowlist),
  not something inherited from the answer.
- Cost: a plugin whose effect must itself satisfy a human-writer guard cannot be
  expressed as a plugin request. That effect needs a core gate (as `resident_authority`
  and `script_grant` are), which is more core code for each such case.

## Requests that decide agent memory are answered only by a human

**Recorded:** 2026-11-07 · [ORB-22824]

### Context

Precedent rules and the decision model answer requests without the human, gated by
risk tier and per-category enablement. Some requests decide what agents will later read
as knowledge or run as automation: scribe's records of Daniel's decisions,
directions and preferences, runbooks, auto-tasks, and skills via
their PRs. An agent or a model trained on the human's past answers that approved such a
request would curate its own memory. [ORB-10736] retired native learnings to prevent
that. scribe's own rules forbid it too: records change only on Daniel's explicit
instruction, with no auto-backfill, and agents cannot settle its proposals.
The real alternative was to treat these like any request: tier them `reversible`,
because a page edit can be reverted, and let a category earn self-answer through
shadow measurement. Measurement would show the model predicts the human well. That is
exactly the loop to avoid, and accuracy does not fix it.

### Decision

A gate whose answer changes agent-readable guidance or standing automation is fixed at
`authority` tier by core at raise, whatever its raiser declares. It is excluded from
curated-rule auto-answer and from the decision corpus, so no operator enablement can
make it self-answering. Lowering this needs a new design for that specific gate. This
applies to `plugin_request` now and to any future gate with the same effect.

### Consequences

- `plugin_request` joins `resident_authority` and `script_grant` as human-only gates,
  and its answers never become precedent.
- Dream skill and runbook PRs need nothing new: `review_approval` is already
  `irreversible` and human-only.
- Cost: every memory or automation proposal waits for Daniel, however routine. No
  measured accuracy will reduce that queue, so volume is controlled at the producer
  (caps, dismissal memory, batching), not by delegation.

## Task References

- [ORB-22824] — designs plugin-raised decision requests with an apply callback and proposes these rules.
- [ORB-21795] — scribe S6: built the consistency routine whose proposals are the first plugin requests.
- [ORB-10736] — retired native learnings, so agents do not curate their own memory.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
