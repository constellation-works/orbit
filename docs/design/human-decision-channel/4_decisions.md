---
title: Human decision channel — Decisions
owner: claude
created_date: 2026-11-07
updated_date: 2026-11-28
last_validated: 2026-11-28
status: Draft
feature: human-decision-channel
doc_role: decisions
type: design
summary: "Decisions: a decision's effect runs on the host that owns its request, a plugin's effect is never written under the human's label, and plugin-raised requests are human-only by construction"
tags: [human-decision-channel, plugins, distributions]
paths: ["crates/orbit-core/src/application/task/decision/**", "crates/orbit-types/src/task/decision/request/**", "crates/orbit-web/src/api/decisions.rs"]
related_features: [plugins, distributions, operator-attention, application-approvals, state-compatibility]
related_artifacts: [ORB-22824]
---

# Human decision channel — Decisions

Record non-obvious decisions here by title. Task references carry provenance; superseded decisions remain in place so their original reasoning stays legible. See [CONVENTIONS.md §4](../CONVENTIONS.md#4-decisions) for the admission rule and required `Cost:` line.

The three entries below govern plugin-raised decision requests
([2_design.md](./2_design.md) §12, originally drafted in [ORB-22824] and updated for
[distributions](../distributions/1_overview.md)). They bind once Daniel accepts that
design. All three are standing rules (Door 2): each governs every future gate, plugin,
distribution or host this channel gains.

## A decision effect runs on the host that owns its request

**Recorded:** 2026-11-07 · [ORB-22824] (updated 2026-11-28 for distributions)

### Context

A decision request is task history, so it lives on the host that owns the task's
workspace. Answering it is a coordination write that a replica checkout or claimed
worker is refused (`coordination_task_write_refusal`). A plugin's effect must run where
the plugin and its workspace data live. The alternative was a forwarded apply: the owner
host records the human's answer, then asks another host to run the effect over a
host-to-host route. That would move delegated-human authority across hosts and add a
cross-host write path with its own failure modes (partition, lost outcome, double
apply).

### Decision

Raise a request only on a task whose effect can run on the same host, and refuse the
raise when it cannot. Never forward an answer or an apply to another host. When work
needs a decision whose effect belongs on a specific host, raise the request on that
host, against a task in a workspace that host owns. Do not move the effect. This
applies to every future gate, plugin, distribution and channel.

### Consequences

- Every decision's authority record, apply and audit sit in one host's history. A
  partition delays a decision and never splits it.
- Distributions and Orbit-plane plugins align naturally with this rule: their
  workspace, plugin install, secrets, channel and data live on the same owner host.
- Cost: a host without a registered channel has no email reach. Federated `orbit
  attention` reads can discover an open request and display a command for each answer,
  but the read itself does not record an answer. The answer must be submitted on the
  owner host through its dashboard or CLI; if that host registers its own channel,
  `decision-mail` is another owner-host answer surface. One mailbox does not forward
  answers across different owner hosts.

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

- Audit reads in two steps, "daniel answered on the dashboard, then plugin `<ns>`
  applied under dr-…", and a human-writer guard can never be satisfied by plugin code.
- The apply ceiling is a separate, explicit operator consent (the apply-tool allowlist),
  not something inherited from the answer or from a distribution preset.
- Cost: a plugin whose effect must itself satisfy a human-writer guard cannot be
  expressed as a plugin request. That effect needs a core gate (as `resident_authority`
  and `script_grant` are), which is more core code for each such case.

## Plugin-raised requests are human-only by construction

**Recorded:** 2026-11-07 · [ORB-22824] (generalized 2026-11-28)

### Context

A plugin's apply tool executes arbitrary mutating domain effects at operator ceiling
within the plugin's grant, and core does not understand what those effects do. Letting
a plugin self-declare a lower risk tier (`routine` or `reversible`) so curated precedent
rules or future automation could answer its requests would let a raiser grade its own
blast radius — and for any plugin touching agent-readable state, standing automation or
external systems, automated answering creates a self-approval loop. (Previously this
rule was framed around Scribe memory proposals; Scribe belongs to the live-session
plane and is not a consumer, while the underlying invariant applies to every
distribution and Orbit-plane plugin.)

### Decision

Core fixes every `plugin_request` answer at `authority` tier at raise, whatever the
raiser declares, and excludes `plugin_request` from precedent auto-answer and precedent
consolidation. Lowering a specific future gate out of human-only requires a dedicated
design and explicit operator opt-in for that gate. This applies to `plugin_request` now
and to any future generic extension gate whose effects core does not interpret.

### Consequences

- `plugin_request` joins `resident_authority` and `script_grant` as human-only gates,
  and its answers never become precedent rules.
- Dream skill and runbook PRs keep using `review_approval`, which is already
  `irreversible` and human-only.
- Cost: every plugin-raised decision waits for a human, however routine. Volume must be
  controlled at the producer (caps, deduplication, digest batching), not by automated
  delegation.

## Task References

- [ORB-22824] — initially designed plugin-raised decision requests with an apply callback and proposed these rules.

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
