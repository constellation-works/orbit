---
title: Human Decision Channel — Overview
owner: claude
created_date: 2026-10-24
updated_date: 2026-11-28
last_validated: 2026-11-24
status: Accepted
stage: shipped
next: "File the plugin-raised request slices (2_design.md §12, designed in ORB-22824); none filed"
feature: human-decision-channel
doc_role: overview
type: design
summary: "A core decision-request record and delegated-human channel, with email as a plugin transport, so held Orbit work can be decided by replying to an email"
tags: [human-decision-channel, plugins, routines, blocked-recovery, task-pilot]
---

# Human Decision Channel — Overview

Status: the committed slices have landed and stay off until an operator enables the
channel. [ORB-17979] authored this design. The folder pairs this overview with [2_design.md](./2_design.md), which carries the mechanisms, and with [4_decisions.md](./4_decisions.md).

## 1. Motivation

Bearing: block triage
(`crates/orbit-core/src/application/task/blocked_recovery/triage.rs`), task-pilot
admission holds (`crates/orbit-core/src/application/task/pilot/admission/backlog.rs`), the
human update path (`update_task_as_human`,
`crates/orbit-core/src/application/task/update/entry.rs`), the authorization catalog
(`crates/orbit-common/src/governance/authorization/`), state and cron triggers
(`crates/orbit-types/src/workflow/automation/members.rs`,
[automation-triggers](../automation-triggers/3_vision.md)), and the plugin standard
([plugins](../plugins/1_overview.md)). Anchors re-checked at `0444775f0` (2026-11-24). The two `authorization/` symbols in
Shape are still `CallerProvenance` and `GOVERNED_OPERATIONS`; [2_design.md](./2_design.md)
cites the other mechanisms. Line numbers were dropped.

### The gap

Orbit already knows when work is waiting on a human, and records that fact in four
different shapes: a block triage that ends in `propose` (2–3 options with a
recommendation) or `report` (`human_action:`); a final-recovery escalation carrying a
`human_action:` line; a task-pilot admission hold waiting for an operator verdict
comment; and the ordinary approval gates (`proposed → backlog`, `review → done`).
None of these reaches the human. There is no outbound notification for blocked tasks
or pilot holds — the desktop watcher (`desktop/src/watcher.rs`) does not cover them —
so a held task sits until someone happens to open the dashboard or a task list. The
hold is durable and well-described; the last mile to the person who can clear it does
not exist.

The return path is the harder half. A human decision counts today only when it is
written by a non-automation writer label through the human surfaces: the dashboard
comment endpoint and the bare CLI, both funnelling into `update_task_as_human`
(`operator_decision_authority: true`). Pilot admission's `operator_resolution`
explicitly discards any resolution whose writer `is_automation_actor`
(`is_automation_actor` in `crates/orbit-types/src/identity/actor.rs`). A plugin backend calling back into
Orbit tools is an automation actor with a plugin provenance; at the surveyed revision
nothing in the plugin callback path
(`crates/orbit-tools/src/plugin/callback/session.rs`) can stamp a human writer label.
So even a perfect email transport built purely as a plugin cannot record the decision
it carried: the authority gap is in core, and closing it without weakening the
agent/automation exclusion is the central design problem. An approval also has real
consequences — it admits agent code execution — so the reply path is a security
boundary, not a convenience shortcut.

### Intended outcome

When work parks on a human decision and the channel is enabled, Orbit raises a
*decision request* naming the task, the gate, and the closed set of allowed answers;
a transport plugin emails it to the operator; the operator replies with one allowed
answer; Orbit authenticates the reply, checks it is still about the same hold, and
records the decision as the human, with the channel's provenance attached. Core owns
the request record, the delegated-human authority, and the apply step; the plugin
owns SMTP, IMAP, and message formatting. Four decisions are settled by the operator
and this design keeps them (re-stated in [2_design.md](./2_design.md) §1): the
core/plugin split with no email specifics in core; a dedicated mailbox with SMTP out
and IMAP polled in, no inbound listener; a closed decision scope (blocked
propose/report choices, pilot-hold verdicts, proposed approve/reject by digest,
review approve) with proposed approvals always pilot-gated and no `--force` or
free-form edits; and dk-server-2 as the host. Everything else about how Orbit runs —
the loopback-only server, the automation exclusion, the no-event-bus rule — stays
unchanged.

### Out of scope

- **No event bus, no resident listener.** Ruled out already
  ([automation-triggers](../automation-triggers/3_vision.md)); inbound mail is polled
  by a cron routine and the server stays loopback-only.
- **No free-form task editing by email.** No field edits, no `--force`, no requeue,
  no criteria changes. The channel picks from a closed answer set on an open request.
- **No other transports in v1.** The core record is transport-neutral, but only the
  email plugin is designed here; a second transport is a later plugin against the
  same core contract.
- **No change to who counts as automation.** `is_automation_actor` and the pilot and
  recovery exclusions keep their meaning; the channel enters through the human path
  with provenance, not by reclassifying a plugin as human.
- **No implementation in this task.** [2_design.md](./2_design.md) ends in ordered
  slices; filing them is the operator's job after review.

### Acceptance (feature level)

- Given a blocked task whose final recovery `decide`d with the channel enabled, exactly one email
  per block episode offers the triage's options; replying `2` records option 2 as the
  human, with channel provenance, and a confirmation mail follows.
- Given a pilot admission hold, replying `approve-anyway` (or its digest number)
  records the same history entry a dashboard verdict comment records, and
  `operator_resolution` accepts it; an agent or automation writer attempting the
  same submission is still refused.
- A reply from a non-allowlisted sender, with failing DKIM/SPF, with a reused or
  expired token, for a superseded episode, or naming an answer outside the allowed
  set changes nothing and produces the refusal behaviour in
  [2_design.md](./2_design.md) §5.
- A proposed task approved from the digest enters task-pilot like any other
  approval; it never lands in `backlog` with an empty footprint.
- With the channel disabled (the default), no requests are raised, no mail is sent,
  and every existing surface behaves as today.

The mechanisms are in [2_design.md](./2_design.md); standing rules are kept in [4_decisions.md](./4_decisions.md).

## 2. Core Concepts

- **Decision request.** The core record raised when work parks on a human decision: it names the task, the gate and the closed set of allowed answers.
- **Delegated-human authority.** The channel path that records an authenticated reply as the human, with the channel's provenance attached, without reclassifying a plugin as human.
- **Transport plugin.** The plugin that owns SMTP, IMAP and message formatting; core owns the request record, the authority and the apply step.
- **Closed answer set.** The only answers a reply may carry: blocked propose/report choices, pilot-hold verdicts, proposed approve/reject by digest, and review approve.
- **Stale guard.** The check that a reply is still about the same hold, so a reply for a superseded episode changes nothing.

## 3. At a Glance

| Concern | File | Task |
|---------|------|------|
| Settled decisions the design keeps | [2_design.md §1](./2_design.md#1-settled-decisions-kept) | [ORB-17979] |
| The decision-request record, states and storage | [2_design.md §2](./2_design.md#2-the-decision-request-record) | [ORB-17979] |
| Delegated-human authority and channel registration | [2_design.md §3](./2_design.md#3-delegated-human-authority) | [ORB-17979] |
| Reply authentication | [2_design.md §4](./2_design.md#4-reply-authentication) | [ORB-17979] |
| Stale guard and refusals | [2_design.md §5](./2_design.md#5-stale-guard-and-refusals) | [ORB-17979] |
| Triggers, digest routine and noise control | [2_design.md §7–8](./2_design.md#7-triggers-and-the-digest-routine) | [ORB-17979] |
| Precedent-answered decisions | [2_design.md §11](./2_design.md#11-precedent-answered-decisions) | [ORB-17979] |
| Plugin-raised requests with an apply callback | [2_design.md §12](./2_design.md#12-plugin-raised-decision-requests-with-an-apply-callback) | [ORB-17979] |
| Standing rules | [4_decisions.md](./4_decisions.md) | — |

## Task References

- [ORB-17979] — designed the email channel for human decisions on held Orbit work (this folder).

> Resolve any task above with `orbit task show <ID>` or `git log --grep=<ID>`.
